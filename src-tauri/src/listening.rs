//! Connects voice to the rest of Luna: spoken requests go through the same path as typed ones,
//! and listening state is reported to the window and tray.
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::assistant::{self, Relevance};
use crate::commands::AppState;
use crate::error::AppError;
use crate::settings::{self, Settings};
use crate::voice::{CaptureError, Host, Timing, VoiceError, VoiceState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct VoiceStatus {
    pub state: VoiceState,
    /// Why listening is off when the user wanted it on.
    pub problem: Option<String>,
}

impl Default for VoiceStatus {
    fn default() -> Self {
        Self {
            state: VoiceState::Off,
            problem: None,
        }
    }
}

const AFFIRMATIVE: &[&str] = &[
    "yes",
    "yeah",
    "yep",
    "sure",
    "do it",
    "go ahead",
    "confirm",
    "ok",
    "okay",
    "please do",
];
const NEGATIVE: &[&str] = &[
    "no",
    "nope",
    "cancel",
    "don't",
    "dont",
    "stop",
    "never mind",
];

/// "Yes" or "no" in answer to a confirmation question.
fn confirmation_answer(text: &str) -> Option<bool> {
    let words: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'')
        .collect();
    let phrase = words.split_whitespace().collect::<Vec<_>>().join(" ");
    let phrase = phrase.trim_end_matches(" please").trim_end_matches(" luna");
    if AFFIRMATIVE.contains(&phrase) {
        Some(true)
    } else if NEGATIVE.contains(&phrase) {
        Some(false)
    } else {
        None
    }
}

struct VoiceHost {
    app: AppHandle,
}

impl Host for VoiceHost {
    fn respond(&self, request: &str) -> String {
        let state = self.app.state::<AppState>();
        let pending = state.pending_confirmation();
        let result = tauri::async_runtime::block_on(async {
            match pending.zip(confirmation_answer(request)) {
                Some((id, confirmed)) => state.confirm(&self.app, id, confirmed).await,
                None => state.ask(&self.app, request).await,
            }
        });
        crate::emit(&self.app, crate::INTERACTIONS_EVENT, ());
        match result {
            Ok(interaction) => interaction.response,
            Err(error) => {
                log::error!("voice request failed: {error}");
                error.user_message()
            }
        }
    }

    fn relevance(&self, text: &str) -> Relevance {
        let state = self.app.state::<AppState>();
        if state.pending_confirmation().is_some() && confirmation_answer(text).is_some() {
            return Relevance::Request;
        }
        assistant::relevance(&state.home_assistant.cache().read(), text)
    }

    fn cancel(&self) {
        self.app.state::<AppState>().cancel_request();
    }

    fn state_changed(&self, state: VoiceState) {
        set_status(&self.app, state, None);
    }

    fn stopped(&self, error: VoiceError) {
        log::warn!("listening stopped: {error}");
        set_status(&self.app, VoiceState::Off, Some(problem(&error)));
    }
}

/// Turns listening on or off from the window or tray and remembers the choice.
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), AppError> {
    let state = app.state::<AppState>();
    let result = if enabled {
        start(app)
    } else {
        stop(app);
        Ok(())
    };
    let settings = Settings {
        listening: enabled && result.is_ok(),
        ..state.settings()?
    };
    settings::save(&state.db(), &settings)?;
    result
}

/// Starts listening with the installed voice models.
pub fn start(app: &AppHandle) -> Result<(), AppError> {
    let state = app.state::<AppState>();
    let Some(files) = state.models.voice().files() else {
        let error = AppError::VoiceModelsMissing;
        set_status(app, VoiceState::Off, Some(error.user_message()));
        return Err(error);
    };
    let host = Arc::new(VoiceHost { app: app.clone() });
    if let Err(error) = state.voice.start(files, Timing::default(), host) {
        set_status(app, VoiceState::Off, Some(problem(&error)));
        return Err(AppError::Voice(error));
    }
    Ok(())
}

/// Stops capture immediately and cancels anything Luna was doing for a spoken request.
pub fn stop(app: &AppHandle) {
    app.state::<AppState>().voice.stop();
    set_status(app, VoiceState::Off, None);
}

fn set_status(app: &AppHandle, state: VoiceState, problem: Option<String>) {
    let status = VoiceStatus { state, problem };
    if app.state::<AppState>().set_voice_status(status) {
        crate::lifecycle::show_listening(app, state != VoiceState::Off);
        crate::emit(app, crate::STATUS_EVENT, app.state::<AppState>().status());
    }
}

pub fn problem(error: &VoiceError) -> String {
    match error {
        VoiceError::Capture(CaptureError::PermissionDenied) => permission_message(),
        VoiceError::Capture(CaptureError::NoDevice) => {
            "No microphone found. Connect one, then turn listening on."
        }
        VoiceError::Capture(CaptureError::Disconnected) => {
            "The microphone was disconnected. Turn listening on when it's back."
        }
        VoiceError::Capture(CaptureError::Failed) => {
            "The microphone couldn't start. Turn listening on to try again."
        }
        VoiceError::ModelLoad(_) | VoiceError::Transcription => {
            "Voice couldn't start. Download the voice models again in Settings."
        }
        VoiceError::Speech => "Spoken replies aren't available on this computer.",
    }
    .to_owned()
}

#[cfg(target_os = "macos")]
fn permission_message() -> &'static str {
    "Luna can't use the microphone. Allow it in System Settings > Privacy & Security > Microphone."
}

#[cfg(not(target_os = "macos"))]
fn permission_message() -> &'static str {
    "Luna can't use the microphone. Allow desktop apps to use it in Settings > Privacy & security > Microphone."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmations_are_answered_by_voice() {
        assert_eq!(confirmation_answer("Yes."), Some(true));
        assert_eq!(confirmation_answer("Yeah, go ahead"), None);
        assert_eq!(confirmation_answer("Go ahead, Luna."), Some(true));
        assert_eq!(confirmation_answer("No."), Some(false));
        assert_eq!(confirmation_answer("Don't."), Some(false));
        assert_eq!(confirmation_answer("Turn off the lights."), None);
    }
}
