use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AppError;

pub const CONTEXT_LENGTH_RANGE: std::ops::RangeInclusive<u32> = 2048..=32_768;

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            home_assistant_url: String::new(),
            active_model: None,
            context_length: 4096,
            listening: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid Home Assistant URL")]
    InvalidHomeAssistantUrl,
    #[error("context length is out of range")]
    ContextLengthOutOfRange,
}

impl SettingsError {
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::InvalidHomeAssistantUrl => {
                "Enter a Home Assistant address like http://homeassistant.local:8123."
            }
            Self::ContextLengthOutOfRange => "Choose a context length between 2048 and 32768.",
        }
    }
}

impl Settings {
    /// Returns a trimmed copy of the settings, or the first validation error.
    pub fn validated(self) -> Result<Self, SettingsError> {
        let settings = Self {
            home_assistant_url: self
                .home_assistant_url
                .trim()
                .trim_end_matches('/')
                .to_owned(),
            ..self
        };
        if !settings.home_assistant_url.is_empty() && !is_http_url(&settings.home_assistant_url) {
            return Err(SettingsError::InvalidHomeAssistantUrl);
        }
        if !CONTEXT_LENGTH_RANGE.contains(&settings.context_length) {
            return Err(SettingsError::ContextLengthOutOfRange);
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
    }

    #[test]
    fn corrupt_settings_are_reported() {
        let conn = db::open_in_memory().unwrap();
        conn.execute("INSERT INTO settings (id, data) VALUES (1, 'not json')", [])
            .unwrap();
        assert!(matches!(load(&conn), Err(AppError::CorruptSettings(_))));
    }
}
