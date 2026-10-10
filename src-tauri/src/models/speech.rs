//! Speech models from the catalogue: wake word, speech detection, and transcription.
//! Every setting that depends on a particular model lives in `models.json`.
use serde::Deserialize;

use super::store::Artifact;

/// A file Luna downloads and verifies against its SHA-256 before using it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadFile {
    pub url: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
}

impl Artifact for DownloadFile {
    fn url(&self) -> &str {
        &self.url
    }

    fn file_name(&self) -> &str {
        &self.file_name
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechCatalog {
    pub wake_word: KeywordModel,
    pub speech_detection: VadModel,
    pub transcription: TranscriptionModel,
    pub speech_output: SpeechOutputModel,
}

impl SpeechCatalog {
    pub fn files(&self) -> [&DownloadFile; 4] {
        [
            &self.wake_word.archive,
            &self.speech_detection.file,
            &self.transcription.archive,
            &self.speech_output.archive,
        ]
    }
}

/// A streaming keyword spotter, shipped as an archive the model files are extracted from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeywordModel {
    pub archive: DownloadFile,
    pub license: String,
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    /// The sentencepiece model whose pieces spell the chosen wake word.
    pub vocabulary: String,
    pub score: f32,
    pub threshold: f32,
}

impl KeywordModel {
    pub fn extracted_files(&self) -> [&str; 5] {
        [
            &self.encoder,
            &self.decoder,
            &self.joiner,
            &self.tokens,
            &self.vocabulary,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VadModel {
    pub file: DownloadFile,
    pub license: String,
    pub threshold: f32,
    pub min_silence_seconds: f32,
    pub min_speech_seconds: f32,
    pub max_speech_seconds: f32,
    pub window_size: i32,
}

/// Offline speech recognition, shipped as an archive the model files are extracted from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionModel {
    pub archive: DownloadFile,
    pub license: String,
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    /// The sherpa-onnx model type, such as `nemo_transducer`.
    pub model_type: String,
}

impl TranscriptionModel {
    pub fn extracted_files(&self) -> [&str; 4] {
        [&self.encoder, &self.decoder, &self.joiner, &self.tokens]
    }
}

/// Kokoro speech synthesis, run by the separate GPL voice helper. The archive's English files and
/// espeak-ng data are extracted; the voices are speakers within one model.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechOutputModel {
    pub archive: DownloadFile,
    pub license: String,
    pub model: String,
    pub voices_file: String,
    pub tokens: String,
    pub data_dir: String,
    pub speed: f32,
    pub voices: Vec<Voice>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub accent: String,
    pub speaker: i32,
    pub lexicon: String,
    pub lang: String,
}

impl SpeechOutputModel {
    pub fn extracted_files(&self) -> Vec<&str> {
        let mut files = vec![
            self.model.as_str(),
            self.voices_file.as_str(),
            self.tokens.as_str(),
        ];
        for voice in &self.voices {
            if !files.contains(&voice.lexicon.as_str()) {
                files.push(&voice.lexicon);
            }
        }
        files
    }

    pub fn voice(&self, id: &str) -> Option<&Voice> {
        self.voices.iter().find(|voice| voice.id == id)
    }
}
