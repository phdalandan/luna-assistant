use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AppError;

pub const CONTEXT_LENGTH_RANGE: std::ops::RangeInclusive<u32> = 2048..=131_072;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Empty until the user connects Home Assistant.
    pub home_assistant_url: String,
    pub ollama_url: String,
    pub model: String,
    pub context_length: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            home_assistant_url: String::new(),
            ollama_url: "http://127.0.0.1:11434".into(),
            model: "qwen3:8b".into(),
            context_length: 8192,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid Home Assistant URL")]
    InvalidHomeAssistantUrl,
    #[error("invalid Ollama URL")]
    InvalidOllamaUrl,
    #[error("model name is empty")]
    EmptyModel,
    #[error("context length is out of range")]
    ContextLengthOutOfRange,
}

impl SettingsError {
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::InvalidHomeAssistantUrl => {
                "Enter a Home Assistant address like http://homeassistant.local:8123."
            }
            Self::InvalidOllamaUrl => "Enter an Ollama address like http://127.0.0.1:11434.",
            Self::EmptyModel => "Enter a model name.",
            Self::ContextLengthOutOfRange => "Choose a context length between 2048 and 131072.",
        }
    }
}

impl Settings {
    /// Returns a trimmed copy of the settings, or the first validation error.
    pub fn validated(self) -> Result<Self, SettingsError> {
        let settings = Self {
            home_assistant_url: self.home_assistant_url.trim().to_owned(),
            ollama_url: self.ollama_url.trim().to_owned(),
            model: self.model.trim().to_owned(),
            context_length: self.context_length,
        };

        if !settings.home_assistant_url.is_empty() && !is_http_url(&settings.home_assistant_url) {
            return Err(SettingsError::InvalidHomeAssistantUrl);
        }
        if !is_http_url(&settings.ollama_url) {
            return Err(SettingsError::InvalidOllamaUrl);
        }
        if settings.model.is_empty() {
            return Err(SettingsError::EmptyModel);
        }
        if !CONTEXT_LENGTH_RANGE.contains(&settings.context_length) {
            return Err(SettingsError::ContextLengthOutOfRange);
        }
        Ok(settings)
    }
}

fn is_http_url(value: &str) -> bool {
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
    }

    #[test]
    fn validation_trims_values() {
        let settings = Settings {
            home_assistant_url: "  https://ha.example.com  ".into(),
            model: " gemma3:12b ".into(),
            ..valid()
        };
        let validated = settings.validated().unwrap();
        assert_eq!(validated.home_assistant_url, "https://ha.example.com");
        assert_eq!(validated.model, "gemma3:12b");
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
    fn rejects_invalid_ollama_url() {
        let settings = Settings {
            ollama_url: String::new(),
            ..valid()
        };
        assert_eq!(settings.validated(), Err(SettingsError::InvalidOllamaUrl));
    }

    #[test]
    fn rejects_empty_model() {
        let settings = Settings {
            model: "   ".into(),
            ..valid()
        };
        assert_eq!(settings.validated(), Err(SettingsError::EmptyModel));
    }

    #[test]
    fn rejects_context_length_out_of_range() {
        for context_length in [0, 2047, 131_073] {
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
    fn save_then_load_round_trips() {
        let conn = db::open_in_memory().unwrap();
        save(&conn, &valid()).unwrap();
        let updated = Settings {
            model: "gemma3:12b".into(),
            ..valid()
        };
        save(&conn, &updated).unwrap();
        assert_eq!(load(&conn).unwrap(), updated);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let conn = db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO settings (id, data) VALUES (1, ?1)",
            [r#"{"model":"gemma3:12b"}"#],
        )
        .unwrap();
        let settings = load(&conn).unwrap();
        assert_eq!(settings.model, "gemma3:12b");
        assert_eq!(settings.ollama_url, Settings::default().ollama_url);
    }

    #[test]
    fn corrupt_settings_are_reported() {
        let conn = db::open_in_memory().unwrap();
        conn.execute("INSERT INTO settings (id, data) VALUES (1, 'not json')", [])
            .unwrap();
        assert!(matches!(load(&conn), Err(AppError::CorruptSettings(_))));
    }
}
