use serde::Serialize;

use crate::settings::SettingsError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    InvalidSettings(#[from] SettingsError),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than supported version {supported}")]
    UnsupportedDatabaseVersion { found: u32, supported: u32 },
    #[error("stored settings could not be parsed: {0}")]
    CorruptSettings(#[source] serde_json::Error),
    #[error("launch at login could not be changed: {0}")]
    LaunchAtLogin(String),
}

impl AppError {
    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidSettings(error) => error.user_message().to_owned(),
            Self::Database(_) => {
                "Luna couldn't access its data. Restart Luna and try again.".into()
            }
            Self::UnsupportedDatabaseVersion { .. } => {
                "This data was created by a newer version of Luna. Update Luna to continue.".into()
            }
            Self::CorruptSettings(_) => {
                "Saved settings couldn't be read. Saving your settings again will replace them."
                    .into()
            }
            Self::LaunchAtLogin(_) => {
                "Launch at login couldn't be changed. Check your system's login item settings."
                    .into()
            }
        }
    }
}

/// The only error shape sent to the frontend. Technical details stay in the logs.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct CommandError {
    pub message: String,
}

impl From<AppError> for CommandError {
    fn from(error: AppError) -> Self {
        log::error!("{error}");
        Self {
            message: error.user_message(),
        }
    }
}
