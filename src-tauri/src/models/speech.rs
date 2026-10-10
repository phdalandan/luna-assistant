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
}

impl SpeechCatalog {
    pub fn files(&self) -> [&DownloadFile; 3] {
        [
            &self.wake_word.archive,
            &self.speech_detection.file,
            &self.transcription.file,
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
    /// The wake word in the model's own tokens, as sherpa-onnx expects.
    pub keyword: String,
    pub score: f32,
    pub threshold: f32,
}

impl KeywordModel {
    pub fn extracted_files(&self) -> [&str; 4] {
        [&self.encoder, &self.decoder, &self.joiner, &self.tokens]
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionModel {
    pub file: DownloadFile,
    pub license: String,
    pub language: String,
}
