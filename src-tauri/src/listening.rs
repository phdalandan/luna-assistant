//! Connects voice to the rest of Luna: spoken requests go through the same path as typed ones,
//! and listening state is reported to the window and tray.
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::assistant::{self, Relevance};
use crate::commands::AppState;
use crate::error::AppError;
use crate::settings::InferenceMode;
use crate::settings::{self, Settings};
use crate::voice::{
    self, CaptureError, Host, SAMPLE_RATE, Timing, TranscriptionFailed, VoiceError, VoiceSettings,
    VoiceState,
};

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

struct VoiceHost {
    app: AppHandle,
}

impl Host for VoiceHost {
    fn respond(&self, request: &str) -> String {
        let state = self.app.state::<AppState>();
        let pending = state.pending_confirmation();
        let result = tauri::async_runtime::block_on(async {
            match pending.zip(assistant::yes_or_no(request)) {
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
        let awaiting_answer = state.pending_confirmation().is_some() || state.has_offer();
        if awaiting_answer && assistant::yes_or_no(text).is_some() {
            return Relevance::Request;
        }
        assistant::relevance(&state.home_assistant.cache().read(), text)
    }

    fn vocabulary(&self) -> String {
        let state = self.app.state::<AppState>();
        assistant::vocabulary(&state.home_assistant.cache().read())
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String, TranscriptionFailed> {
        let state = self.app.state::<AppState>();
        let vocabulary = self.vocabulary();
        let prompt = voice::vocabulary_prompt(&vocabulary);
        tauri::async_runtime::block_on(state.transcribe(samples, SAMPLE_RATE, prompt)).map_err(
            |error| {
                log::error!("cloud transcription failed: {error}");
                TranscriptionFailed {
                    reply: error.user_message(),
                }
            },
        )
    }

    fn cancel(&self) {
        self.app.state::<AppState>().cancel_request();
    }

    fn audio_level(&self, level: f32) {
        if self.app.state::<AppState>().window_visible() {
            crate::emit(&self.app, crate::LEVEL_EVENT, level);
        }
    }

    fn state_changed(&self, state: VoiceState) {
        set_status(&self.app, state, None);
    }

    fn stopped(&self, error: VoiceError) {
        log::warn!("listening stopped: {error}");
        set_status(&self.app, VoiceState::Off, Some(problem(&error)));
    }
}

/// Starting and stopping wait for the voice thread, which reports to the main thread, so this
/// never runs on the main thread.
pub async fn set_enabled_in_background(app: AppHandle, enabled: bool) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || set_enabled(&app, enabled))
        .await
        .unwrap_or_else(|error| {
            log::error!("listening change did not finish: {error}");
            Err(AppError::Voice(VoiceError::Capture(CaptureError::Failed)))
        })
}

/// Clears "download the voice models first" once they are installed.
pub fn models_changed(app: &AppHandle) {
    let state = app.state::<AppState>();
    let missing = AppError::VoiceModelsMissing.user_message();
    let status = state.status().voice;
    if status.problem.as_deref() == Some(missing.as_str()) && state.models.voice().files().is_some()
    {
        set_status(app, status.state, None);
    }
}

/// Turns listening on or off and remembers the choice. Any failure is reported through the voice
/// status, which the window and tray show.
fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), AppError> {
    let result = switch(app, enabled);
    if let Err(error) = &result {
        let state = app.state::<AppState>().status().voice.state;
        set_status(app, state, Some(error.user_message()));
    }
    result
}

fn switch(app: &AppHandle, enabled: bool) -> Result<(), AppError> {
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
    let saved = state.settings()?;
    let started = voice::bundled_helper()
        .map_err(|error| {
            log::error!("could not find the voice helper: {error}");
            VoiceError::Speech
        })
        .and_then(|helper| {
            let settings = VoiceSettings {
                wake_word: saved.wake_word,
                microphone: saved.microphone,
                voice: saved.voice,
                helper,
                cloud_transcription: saved.speech_recognition == InferenceMode::Cloud,
            };
            let host = Arc::new(VoiceHost { app: app.clone() });
            state.voice.start(files, settings, Timing::default(), host)
        });
    if let Err(error) = started {
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
        VoiceError::Capture(CaptureError::ChosenDeviceMissing) => {
            "The chosen microphone isn't connected. Connect it or choose another in Settings."
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
        VoiceError::Speech => {
            "Luna's voice couldn't start. Download the voice models again in Settings."
        }
        VoiceError::WakeWord => "Luna can't listen for that wake phrase. Try another in Settings.",
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
