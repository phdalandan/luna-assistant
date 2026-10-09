use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Deserialize;

const CATALOG: &str = include_str!("../../models.json");
const TRUSTED_HOST: &str = "huggingface.co";

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

/// Model-specific generation settings. All model-specific behaviour lives here.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatOptions {
    pub disable_thinking: bool,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    models: Vec<CatalogModel>,
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
}

pub fn models() -> &'static [CatalogModel] {
    static MODELS: OnceLock<Vec<CatalogModel>> = OnceLock::new();
    MODELS.get_or_init(|| parse(CATALOG).expect("bundled model catalog is valid"))
}

#[cfg(test)]
pub fn find(id: &str) -> Option<&'static CatalogModel> {
    models().iter().find(|model| model.id == id)
}

fn parse(json: &str) -> Result<Vec<CatalogModel>, CatalogError> {
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
        let safe_name = model
            .file_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !safe_name || !model.file_name.ends_with(".gguf") || model.file_name.starts_with('.') {
            return Err(CatalogError::InvalidFileName(model.id.clone()));
        }
        let is_hex = model.sha256.len() == 64
            && model
                .sha256
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if !is_hex {
            return Err(CatalogError::InvalidChecksum(model.id.clone()));
        }
        if model.size == 0 {
            return Err(CatalogError::InvalidSize(model.id.clone()));
        }
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
    Ok(catalog.models)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn catalog_with(change: impl FnOnce(&mut Value)) -> Result<Vec<CatalogModel>, CatalogError> {
        let mut value: Value = serde_json::from_str(CATALOG).unwrap();
        change(&mut value);
        parse(&value.to_string())
    }

    #[test]
    fn bundled_catalog_is_valid() {
        let models = parse(CATALOG).unwrap();
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
    fn requires_one_recommended_model() {
        let result = catalog_with(|catalog| catalog["models"][1]["recommended"] = json!(true));
        assert_eq!(result, Err(CatalogError::Recommendation));
    }
}
