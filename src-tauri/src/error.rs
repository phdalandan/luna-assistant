use serde::Serialize;

use crate::assistant::AssistantError;
use crate::credentials::CredentialError;
use crate::home_assistant::discovery::DiscoveryError;
use crate::inference::InferenceError;
use crate::models::ModelError;
use crate::settings::SettingsError;
use crate::voice::VoiceError;

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
    #[error(transparent)]
    Credentials(#[from] CredentialError),
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Inference(#[from] InferenceError),
    #[error(transparent)]
    Assistant(#[from] AssistantError),
    #[error("no AI model is selected")]
    NoActiveModel,
    #[error("voice models are not installed")]
    VoiceModelsMissing,
    #[error("listening could not start: {0}")]
    Voice(VoiceError),
}

impl AppError {
    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidSettings(error) => error.user_message(),
            Self::Database(_) => "Luna couldn't access its data. Restart Luna and try again.",
            Self::UnsupportedDatabaseVersion { .. } => {
                "This data was created by a newer version of Luna. Update Luna to continue."
            }
            Self::CorruptSettings(_) => {
                "Saved settings couldn't be read. Saving your settings again will replace them."
            }
            Self::LaunchAtLogin(_) => {
                "Launch at login couldn't be changed. Check your system's login item settings."
            }
            Self::Credentials(_) => "Luna couldn't access your saved access token. Try again.",
            Self::Discovery(_) => "Couldn't search your network. Enter the address instead.",
            Self::Model(error) => model_message(error),
            Self::Inference(error) => inference_message(error),
            Self::Assistant(error) => assistant_message(error),
            Self::NoActiveModel => "Choose an AI model in Settings to get started.",
            Self::VoiceModelsMissing => "Download the voice models in Settings first.",
            Self::Voice(error) => return crate::listening::problem(error),
        }
        .to_owned()
    }
}

fn model_message(error: &ModelError) -> &'static str {
    match error {
        ModelError::Unknown(_) | ModelError::NotInstalled(_) => {
            "This model isn't installed. Download it in Settings."
        }
        ModelError::AlreadyDownloading(_) => "This model is already downloading.",
        ModelError::InUse(_) => "Switch to another model before deleting this one.",
        ModelError::Io(_) => "Couldn't update the model files. Check that your disk is available.",
    }
}

fn inference_message(error: &InferenceError) -> &'static str {
    match error {
        InferenceError::RuntimeMissing(_) => "Luna's AI engine is missing. Reinstall Luna.",
        InferenceError::ModelMissing(_) => "This model isn't installed. Download it in Settings.",
        InferenceError::LoadFailed(_) => "Unable to load this model.",
        InferenceError::Timeout => "The AI model took too long to respond. Try again.",
        InferenceError::Request(_) | InferenceError::InvalidResponse(_) => {
            "The AI model couldn't answer. Try again."
        }
    }
}

fn assistant_message(error: &AssistantError) -> &'static str {
    match error {
        AssistantError::Inference(error) => inference_message(error),
        AssistantError::TooManySteps => "Luna couldn't work that out. Try rephrasing.",
        AssistantError::Cancelled => "Stopped.",
        AssistantError::Busy => "Luna is still working on your last request.",
        AssistantError::ConfirmationExpired => "That request expired. Ask again.",
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

macro_rules! command_error_from {
    ($($source:ty),*) => {
        $(impl From<$source> for CommandError {
            fn from(error: $source) -> Self {
                AppError::from(error).into()
            }
        })*
    };
}

command_error_from!(
    SettingsError,
    rusqlite::Error,
    CredentialError,
    DiscoveryError,
    ModelError,
    InferenceError,
    AssistantError
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_never_contain_em_dashes_or_raw_details() {
        let errors = [
            AppError::from(InferenceError::LoadFailed(
                "exit status 1: bad magic".into(),
            )),
            AppError::from(ModelError::InUse("qwen3-8b".into())),
            AppError::from(AssistantError::TooManySteps),
            AppError::NoActiveModel,
        ];
        for error in errors {
            let message = error.user_message();
            assert!(!message.contains('\u{2014}'));
            assert!(!message.contains("qwen3-8b") && !message.contains("magic"));
        }
    }
}
