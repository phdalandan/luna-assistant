use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AppError;
use crate::models::{CloudProvider, catalog};
use crate::voice;

pub const CONTEXT_LENGTH_RANGE: std::ops::RangeInclusive<u32> = 2048..=32_768;

/// Where the AI model runs. Cloud is used only after the user chooses it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "lowercase")]
pub enum InferenceMode {
    #[default]
    Local,
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Empty until the user connects Home Assistant.
    pub home_assistant_url: String,
    /// Changed only by selecting an installed model, never by saving the form.
    pub active_model: Option<String>,
    pub context_length: u32,
    /// Changed only from the listening control, never by saving the form.
    pub listening: bool,
    pub wake_word: String,
    /// A voice id from the catalogue.
    pub voice: String,
    /// Changed only from the Local and Cloud control, never by saving the form.
    pub inference: InferenceMode,
    /// Where spoken requests are transcribed. Changed only from its own control.
    pub speech_recognition: InferenceMode,
    pub cloud_provider: CloudProvider,
    pub openai_model: String,
    pub anthropic_model: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            home_assistant_url: String::new(),
            active_model: None,
            context_length: 4096,
            listening: false,
            wake_word: "Luna".into(),
            voice: "af_heart".into(),
            inference: InferenceMode::Local,
            speech_recognition: InferenceMode::Local,
            cloud_provider: CloudProvider::OpenAi,
            openai_model: catalog::default_cloud_model(CloudProvider::OpenAi),
            anthropic_model: catalog::default_cloud_model(CloudProvider::Anthropic),
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid Home Assistant URL")]
    InvalidHomeAssistantUrl,
    #[error("context length is out of range")]
    ContextLengthOutOfRange,
    #[error("invalid wake word")]
    InvalidWakeWord,
    #[error("unknown voice")]
    UnknownVoice,
    #[error("unknown cloud model")]
    UnknownCloudModel,
}

impl SettingsError {
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::InvalidHomeAssistantUrl => {
                "Enter a Home Assistant address like http://homeassistant.local:8123."
            }
            Self::ContextLengthOutOfRange => "Choose a context length between 2048 and 32768.",
            Self::InvalidWakeWord => {
                "Use a wake word of one to three words with letters only, like Luna."
            }
            Self::UnknownVoice => "Choose one of the listed voices.",
            Self::UnknownCloudModel => "Choose one of the listed models.",
        }
    }
}

impl Settings {
    pub fn cloud_model(&self, provider: CloudProvider) -> &str {
        match provider {
            CloudProvider::OpenAi => &self.openai_model,
            CloudProvider::Anthropic => &self.anthropic_model,
        }
    }

    /// Applies the saved form. Fields with their own controls keep their current values.
    pub fn from_form(form: Self, current: &Self) -> Result<Self, SettingsError> {
        Ok(Self {
            active_model: current.active_model.clone(),
            listening: current.listening,
            inference: current.inference,
            speech_recognition: current.speech_recognition,
            ..form.validated()?
        })
    }

    /// Returns a trimmed copy of the settings, or the first validation error.
    pub fn validated(self) -> Result<Self, SettingsError> {
        let settings = Self {
            home_assistant_url: self
                .home_assistant_url
                .trim()
                .trim_end_matches('/')
                .to_owned(),
            wake_word: self
                .wake_word
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            ..self
        };
        if !settings.home_assistant_url.is_empty() && !is_http_url(&settings.home_assistant_url) {
            return Err(SettingsError::InvalidHomeAssistantUrl);
        }
        if !CONTEXT_LENGTH_RANGE.contains(&settings.context_length) {
            return Err(SettingsError::ContextLengthOutOfRange);
        }
        if !voice::is_valid_wake_word(&settings.wake_word) {
            return Err(SettingsError::InvalidWakeWord);
        }
        if catalog::speech()
            .speech_output
            .voice(&settings.voice)
            .is_none()
        {
            return Err(SettingsError::UnknownVoice);
        }
        if CloudProvider::ALL.iter().any(|provider| {
            catalog::cloud_model(*provider, settings.cloud_model(*provider)).is_none()
        }) {
            return Err(SettingsError::UnknownCloudModel);
        }
        Ok(settings)
    }
}

pub fn is_http_url(value: &str) -> bool {
    Url::parse(value)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

pub fn load(conn: &Connection) -> Result<Settings, AppError> {
    let data: Option<String> = conn
        .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?;
    match data {
        Some(json) => serde_json::from_str(&json).map_err(AppError::CorruptSettings),
        None => Ok(Settings::default()),
    }
}

pub fn save(conn: &Connection, settings: &Settings) -> Result<(), AppError> {
    let json = serde_json::to_string(settings).map_err(AppError::CorruptSettings)?;
    conn.execute(
        "INSERT INTO settings (id, data) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET data = excluded.data",
        params![json],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn valid() -> Settings {
        Settings {
            home_assistant_url: "http://homeassistant.local:8123".into(),
            ..Settings::default()
        }
    }

    #[test]
    fn defaults_are_valid() {
        assert_eq!(Settings::default().validated(), Ok(Settings::default()));
        assert_eq!(Settings::default().context_length, 4096);
        assert_eq!(Settings::default().active_model, None);
    }

    #[test]
    fn validation_trims_addresses() {
        let settings = Settings {
            home_assistant_url: "  https://ha.example.com/  ".into(),
            ..valid()
        };
        assert_eq!(
            settings.validated().unwrap().home_assistant_url,
            "https://ha.example.com"
        );
    }

    #[test]
    fn rejects_invalid_home_assistant_url() {
        for url in ["homeassistant.local", "ftp://ha.local", "http://"] {
            let settings = Settings {
                home_assistant_url: url.into(),
                ..valid()
            };
            assert_eq!(
                settings.validated(),
                Err(SettingsError::InvalidHomeAssistantUrl),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_context_length_out_of_range() {
        for context_length in [0, 2047, 32_769] {
            let settings = Settings {
                context_length,
                ..valid()
            };
            assert_eq!(
                settings.validated(),
                Err(SettingsError::ContextLengthOutOfRange)
            );
        }
    }

    #[test]
    fn load_returns_defaults_when_nothing_is_saved() {
        let conn = db::open_in_memory().unwrap();
        assert_eq!(load(&conn).unwrap(), Settings::default());
    }

    #[test]
    fn selected_model_persists() {
        let conn = db::open_in_memory().unwrap();
        save(&conn, &valid()).unwrap();
        let updated = Settings {
            active_model: Some("gemma-3-12b".into()),
            ..valid()
        };
        save(&conn, &updated).unwrap();
        assert_eq!(load(&conn).unwrap(), updated);
    }

    #[test]
    fn settings_from_older_versions_load_with_defaults() {
        let conn = db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO settings (id, data) VALUES (1, ?1)",
            [r#"{"homeAssistantUrl":"http://ha.local","ollamaUrl":"http://x","model":"qwen3:8b"}"#],
        )
        .unwrap();
        let settings = load(&conn).unwrap();
        assert_eq!(settings.home_assistant_url, "http://ha.local");
        assert_eq!(settings.active_model, None);
        assert_eq!(settings.context_length, 4096);
        assert_eq!(settings.inference, InferenceMode::Local);
    }

    #[test]
    fn corrupt_settings_are_reported() {
        let conn = db::open_in_memory().unwrap();
        conn.execute("INSERT INTO settings (id, data) VALUES (1, 'not json')", [])
            .unwrap();
        assert!(matches!(load(&conn), Err(AppError::CorruptSettings(_))));
    }

    #[test]
    fn wake_words_are_tidied_and_checked() {
        let settings = Settings {
            wake_word: "  Hey   Jarvis ".into(),
            ..valid()
        };
        assert_eq!(settings.validated().unwrap().wake_word, "Hey Jarvis");
        for wake_word in ["", "R2D2", "Luna!", "one two three four"] {
            let settings = Settings {
                wake_word: wake_word.into(),
                ..valid()
            };
            assert_eq!(
                settings.validated(),
                Err(SettingsError::InvalidWakeWord),
                "{wake_word}"
            );
        }
    }

    #[test]
    fn new_installations_use_local_inference() {
        let settings = Settings::default();
        assert_eq!(settings.inference, InferenceMode::Local);
        assert_eq!(settings.speech_recognition, InferenceMode::Local);
        assert_eq!(settings.cloud_model(CloudProvider::OpenAi), "gpt-6-luna");
        assert_eq!(
            settings.cloud_model(CloudProvider::Anthropic),
            "claude-haiku-5-5"
        );
    }

    #[test]
    fn inference_choices_persist() {
        let conn = db::open_in_memory().unwrap();
        let settings = Settings {
            inference: InferenceMode::Cloud,
            cloud_provider: CloudProvider::Anthropic,
            openai_model: "gpt-6-sol".into(),
            anthropic_model: "claude-sonnet-5".into(),
            ..valid()
        };
        save(&conn, &settings).unwrap();
        assert_eq!(load(&conn).unwrap(), settings);
        let json: String = conn
            .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(json.contains(
            r#""inference":"cloud","speechRecognition":"local","cloudProvider":"anthropic""#
        ));
    }

    #[test]
    fn only_catalogue_cloud_models_can_be_chosen() {
        let settings = Settings {
            openai_model: "claude-haiku-5-5".into(),
            ..valid()
        };
        assert_eq!(settings.validated(), Err(SettingsError::UnknownCloudModel));
        let settings = Settings {
            anthropic_model: "Claude Haiku 5.5".into(),
            ..valid()
        };
        assert_eq!(settings.validated(), Err(SettingsError::UnknownCloudModel));
    }

    #[test]
    fn saving_the_form_keeps_the_inference_mode_and_model() {
        let current = Settings {
            inference: InferenceMode::Cloud,
            speech_recognition: InferenceMode::Cloud,
            active_model: Some("qwen3-8b".into()),
            listening: true,
            ..valid()
        };
        let form = Settings {
            cloud_provider: CloudProvider::Anthropic,
            ..valid()
        };
        let saved = Settings::from_form(form, &current).unwrap();
        assert_eq!(saved.inference, InferenceMode::Cloud);
        assert_eq!(saved.speech_recognition, InferenceMode::Cloud);
        assert_eq!(saved.active_model.as_deref(), Some("qwen3-8b"));
        assert!(saved.listening);
        assert_eq!(saved.cloud_provider, CloudProvider::Anthropic);
    }

    #[test]
    fn only_catalogue_voices_can_be_chosen() {
        let settings = Settings {
            voice: "bm_george".into(),
            ..valid()
        };
        assert!(settings.validated().is_ok());
        let settings = Settings {
            voice: "someone_else".into(),
            ..valid()
        };
        assert_eq!(settings.validated(), Err(SettingsError::UnknownVoice));
    }
}
