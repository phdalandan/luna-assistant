use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::speech::{DownloadFile, SpeechCatalog};
use super::store::Artifact;

const CATALOG: &str = include_str!("../../models.json");
const TRUSTED_HOST: &str = "huggingface.co";
/// Speech models published only as sherpa-onnx release assets. Checksums still apply.
const TRUSTED_RELEASES: (&str, &str) = ("github.com", "/k2-fsa/sherpa-onnx/releases/download/");

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub quantization: String,
    pub recommended: bool,
    pub url: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub license: String,
    pub max_context: u32,
    /// KV cache size per context token, used to estimate memory use.
    pub kv_bytes_per_token: u64,
    pub warning: Option<String>,
    pub chat: ChatOptions,
}

impl Artifact for CatalogModel {
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

/// Model-specific generation settings. All model-specific behaviour lives here.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatOptions {
    pub disable_thinking: bool,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "lowercase")]
pub enum CloudProvider {
    OpenAi,
    Anthropic,
}

impl CloudProvider {
    pub const ALL: [Self; 2] = [Self::OpenAi, Self::Anthropic];

    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

/// A cloud model Luna supports, with its verified API model ID.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudModel {
    pub provider: CloudProvider,
    pub id: String,
    pub name: String,
    /// Model-specific request fields, such as reasoning or thinking settings.
    pub request: Map<String, Value>,
}

/// OpenAI speech-to-text, for users who choose cloud speech recognition. Anthropic offers none.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudTranscription {
    pub id: String,
    /// Model-specific form fields, such as language hints.
    pub form: BTreeMap<String, String>,
}

const RESERVED_FORM_FIELDS: [&str; 5] = ["model", "file", "prompt", "response_format", "stream"];

/// Request fields Luna sets itself, which a catalogue entry must never override.
const RESERVED_REQUEST_FIELDS: [&str; 10] = [
    "model",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "max_tokens",
    "max_completion_tokens",
    "store",
    "stream",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    models: Vec<CatalogModel>,
    cloud: Vec<CloudModel>,
    cloud_transcription: CloudTranscription,
    speech: SpeechCatalog,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog is not valid JSON: {0}")]
    Json(String),
    #[error("duplicate model id {0}")]
    DuplicateId(String),
    #[error("model {0} has an invalid download URL")]
    InvalidUrl(String),
    #[error("model {0} has an invalid file name")]
    InvalidFileName(String),
    #[error("model {0} has an invalid checksum")]
    InvalidChecksum(String),
    #[error("model {0} has an invalid size")]
    InvalidSize(String),
    #[error("exactly one model must be recommended")]
    Recommendation,
    #[error("cloud model {0} is invalid")]
    InvalidCloudModel(String),
    #[error("every cloud provider needs at least one model")]
    MissingCloudModels,
}

fn catalog() -> &'static Catalog {
    static PARSED: OnceLock<Catalog> = OnceLock::new();
    PARSED.get_or_init(|| parse(CATALOG).expect("bundled model catalog is valid"))
}

pub fn models() -> &'static [CatalogModel] {
    &catalog().models
}

pub fn speech() -> &'static SpeechCatalog {
    &catalog().speech
}

pub fn cloud_models() -> &'static [CloudModel] {
    &catalog().cloud
}

pub fn cloud_transcription() -> &'static CloudTranscription {
    &catalog().cloud_transcription
}

pub fn cloud_model(provider: CloudProvider, id: &str) -> Option<&'static CloudModel> {
    cloud_models()
        .iter()
        .find(|model| model.provider == provider && model.id == id)
}

/// The first listed model of each provider is its default.
pub fn default_cloud_model(provider: CloudProvider) -> String {
    cloud_models()
        .iter()
        .find(|model| model.provider == provider)
        .map(|model| model.id.clone())
        .expect("bundled catalogue lists a model for every provider")
}

#[cfg(test)]
pub fn find(id: &str) -> Option<&'static CatalogModel> {
    models().iter().find(|model| model.id == id)
}

fn parse(json: &str) -> Result<Catalog, CatalogError> {
    let catalog: Catalog =
        serde_json::from_str(json).map_err(|error| CatalogError::Json(error.to_string()))?;
    let mut ids = HashSet::new();
    for model in &catalog.models {
        if !ids.insert(model.id.as_str()) {
            return Err(CatalogError::DuplicateId(model.id.clone()));
        }
        let url = url::Url::parse(&model.url).ok();
        if !url.is_some_and(|url| url.scheme() == "https" && url.host_str() == Some(TRUSTED_HOST)) {
            return Err(CatalogError::InvalidUrl(model.id.clone()));
        }
        if !safe_file_name(&model.file_name) || !model.file_name.ends_with(".gguf") {
            return Err(CatalogError::InvalidFileName(model.id.clone()));
        }
        check_download(&model.id, &model.sha256, model.size)?;
    }
    for file in catalog.speech.files() {
        check_speech_file(file)?;
    }
    let wake_word = &catalog.speech.wake_word;
    if !wake_word.extracted_files().into_iter().all(safe_file_name) {
        return Err(CatalogError::InvalidFileName(
            wake_word.archive.file_name.clone(),
        ));
    }
    let output = &catalog.speech.speech_output;
    let mut voice_ids = HashSet::new();
    let safe_output = output.extracted_files().into_iter().all(safe_file_name)
        && safe_file_name(&output.data_dir)
        && !output.voices.is_empty()
        && output
            .voices
            .iter()
            .all(|voice| voice_ids.insert(voice.id.as_str()));
    if !safe_output {
        return Err(CatalogError::InvalidFileName(
            output.archive.file_name.clone(),
        ));
    }
    if catalog
        .models
        .iter()
        .filter(|model| model.recommended)
        .count()
        != 1
    {
        return Err(CatalogError::Recommendation);
    }
    check_cloud_models(&catalog.cloud)?;
    let transcription = &catalog.cloud_transcription;
    if transcription.id.is_empty()
        || transcription
            .form
            .keys()
            .any(|key| RESERVED_FORM_FIELDS.contains(&key.as_str()))
    {
        return Err(CatalogError::InvalidCloudModel(transcription.id.clone()));
    }
    Ok(catalog)
}

fn check_cloud_models(models: &[CloudModel]) -> Result<(), CatalogError> {
    let mut ids = HashSet::new();
    for model in models {
        let valid = ids.insert((model.provider, model.id.as_str()))
            && !model.id.is_empty()
            && !model.name.is_empty()
            && model
                .request
                .keys()
                .all(|key| !RESERVED_REQUEST_FIELDS.contains(&key.as_str()));
        if !valid {
            return Err(CatalogError::InvalidCloudModel(model.id.clone()));
        }
    }
    if CloudProvider::ALL
        .iter()
        .any(|provider| models.iter().all(|model| model.provider != *provider))
    {
        return Err(CatalogError::MissingCloudModels);
    }
    Ok(())
}

fn safe_file_name(name: &str) -> bool {
    !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn check_download(id: &str, sha256: &str, size: u64) -> Result<(), CatalogError> {
    let is_hex = sha256.len() == 64
        && sha256
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if !is_hex {
        return Err(CatalogError::InvalidChecksum(id.to_owned()));
    }
    if size == 0 {
        return Err(CatalogError::InvalidSize(id.to_owned()));
    }
    Ok(())
}

fn check_speech_file(file: &DownloadFile) -> Result<(), CatalogError> {
    let id = &file.file_name;
    let trusted = url::Url::parse(&file.url).ok().is_some_and(|url| {
        let host = url.host_str();
        let release =
            host == Some(TRUSTED_RELEASES.0) && url.path().starts_with(TRUSTED_RELEASES.1);
        url.scheme() == "https" && (host == Some(TRUSTED_HOST) || release)
    });
    if !trusted {
        return Err(CatalogError::InvalidUrl(id.clone()));
    }
    if !safe_file_name(id) {
        return Err(CatalogError::InvalidFileName(id.clone()));
    }
    check_download(id, &file.sha256, file.size)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn catalog_with(change: impl FnOnce(&mut Value)) -> Result<Vec<CatalogModel>, CatalogError> {
        let mut value: Value = serde_json::from_str(CATALOG).unwrap();
        change(&mut value);
        parse(&value.to_string()).map(|catalog| catalog.models)
    }

    #[test]
    fn bundled_catalog_is_valid() {
        let models = parse(CATALOG).unwrap().models;
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["qwen3-8b", "qwen3-4b", "gemma-3-12b"]);
        assert!(find("qwen3-8b").unwrap().recommended);
        assert!(find("qwen3-8b").unwrap().chat.disable_thinking);
        assert!(find("missing").is_none());
    }

    #[test]
    fn rejects_untrusted_urls() {
        let result = catalog_with(|catalog| {
            catalog["models"][0]["url"] = json!("http://example.com/model.gguf");
        });
        assert_eq!(result, Err(CatalogError::InvalidUrl("qwen3-8b".into())));
    }

    #[test]
    fn rejects_path_traversal_in_file_names() {
        let result = catalog_with(|catalog| {
            catalog["models"][0]["file_name"] = json!("../luna.db.gguf");
        });
        assert_eq!(
            result,
            Err(CatalogError::InvalidFileName("qwen3-8b".into()))
        );
    }

    #[test]
    fn rejects_malformed_checksums() {
        let result = catalog_with(|catalog| catalog["models"][2]["sha256"] = json!("abc"));
        assert_eq!(
            result,
            Err(CatalogError::InvalidChecksum("gemma-3-12b".into()))
        );
    }

    #[test]
    fn rejects_duplicate_ids_and_unknown_fields() {
        let duplicate = catalog_with(|catalog| catalog["models"][1]["id"] = json!("qwen3-8b"));
        assert_eq!(duplicate, Err(CatalogError::DuplicateId("qwen3-8b".into())));
        let unknown = catalog_with(|catalog| catalog["models"][0]["mirror"] = json!("x"));
        assert!(matches!(unknown, Err(CatalogError::Json(_))));
    }

    #[test]
    fn speech_files_must_come_from_trusted_sources() {
        let result = catalog_with(|catalog| {
            catalog["speech"]["speech_detection"]["file"]["url"] =
                json!("https://github.com/someone/else/releases/download/x/silero_vad.onnx");
        });
        assert_eq!(
            result,
            Err(CatalogError::InvalidUrl("silero_vad.onnx".into()))
        );
        let result = catalog_with(|catalog| {
            catalog["speech"]["wake_word"]["tokens"] = json!("../tokens.txt");
        });
        assert!(matches!(result, Err(CatalogError::InvalidFileName(_))));
    }

    #[test]
    fn cloud_models_use_verified_api_ids() {
        let ids: Vec<_> = cloud_models()
            .iter()
            .map(|model| (model.provider, model.id.as_str()))
            .collect();
        assert_eq!(
            ids,
            [
                (CloudProvider::OpenAi, "gpt-6-luna"),
                (CloudProvider::OpenAi, "gpt-6-sol"),
                (CloudProvider::Anthropic, "claude-haiku-5-5"),
                (CloudProvider::Anthropic, "claude-sonnet-5"),
            ]
        );
        assert_eq!(default_cloud_model(CloudProvider::OpenAi), "gpt-6-luna");
        assert_eq!(
            default_cloud_model(CloudProvider::Anthropic),
            "claude-haiku-5-5"
        );
        assert!(cloud_model(CloudProvider::OpenAi, "claude-haiku-5-5").is_none());
    }

    #[test]
    fn cloud_transcription_uses_the_verified_model() {
        assert_eq!(cloud_transcription().id, "gpt-transcribe");
        let result = catalog_with(|catalog| {
            catalog["cloud_transcription"]["form"]["file"] = json!("other.wav");
        });
        assert_eq!(
            result,
            Err(CatalogError::InvalidCloudModel("gpt-transcribe".into()))
        );
    }

    #[test]
    fn cloud_models_cannot_override_luna_request_fields() {
        let result = catalog_with(|catalog| {
            catalog["cloud"][0]["request"]["tools"] = json!([]);
        });
        assert_eq!(
            result,
            Err(CatalogError::InvalidCloudModel("gpt-6-luna".into()))
        );
        let result = catalog_with(|catalog| {
            let cloud = catalog["cloud"].as_array_mut().unwrap();
            cloud.retain(|model| model["provider"] != "anthropic");
        });
        assert_eq!(result, Err(CatalogError::MissingCloudModels));
    }

    #[test]
    fn requires_one_recommended_model() {
        let result = catalog_with(|catalog| catalog["models"][1]["recommended"] = json!(true));
        assert_eq!(result, Err(CatalogError::Recommendation));
    }
}
