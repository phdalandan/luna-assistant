use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::assistant::{self, CONVERSATION_LIFETIME, EngineChat, Home, Session};
use crate::credentials::{self, AccessToken};
use crate::error::{AppError, CommandError};
use crate::history::{self, Interaction};
use crate::home_assistant::discovery::{self, DiscoveredInstance};
use crate::home_assistant::{ConnectionStatus, HomeAssistant};
use crate::inference::{Engine, EngineStatus, ModelSpec, Warmup};
use crate::models::{ModelInfo, ModelManager};
use crate::settings::{self, Settings};

const VISIBLE_HISTORY: usize = 30;

pub struct AppState {
    db: Mutex<Connection>,
    pub home_assistant: HomeAssistant,
    pub engine: Engine,
    pub models: Arc<ModelManager>,
    session: Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub home_assistant: ConnectionStatus,
    pub engine: EngineStatus,
}

impl AppState {
    pub fn new(db: Connection, engine: Engine, models: Arc<ModelManager>) -> Self {
        Self {
            db: Mutex::new(db),
            home_assistant: HomeAssistant::default(),
            engine,
            models,
            session: Session::default(),
        }
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite in a partial state.
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn settings(&self) -> Result<Settings, AppError> {
        settings::load(&self.db())
    }

    pub fn status(&self) -> Status {
        Status {
            home_assistant: self.home_assistant.status(),
            engine: self.engine.status(),
        }
    }

    /// Connects to Home Assistant with the saved address and token.
    pub fn connect_home_assistant(&self, url: &str) {
        if url.is_empty() {
            self.home_assistant.configure(None);
            return;
        }
        match credentials::load_home_assistant_token() {
            Ok(Some(token)) => self.home_assistant.configure(Some((url.to_owned(), token))),
            Ok(None) => self.home_assistant.configure(None),
            Err(error) => {
                log::error!("{error}");
                self.home_assistant.fail(ConnectionStatus::TokenUnavailable);
            }
        }
    }

    fn model_spec(&self, settings: &Settings) -> Result<ModelSpec, AppError> {
        let id = settings
            .active_model
            .as_deref()
            .ok_or(AppError::NoActiveModel)?;
        let model = self.models.installed(id)?;
        Ok(ModelSpec {
            id: model.id.clone(),
            path: self.models.store().model_path(model),
            context_length: settings.context_length,
            chat: model.chat.clone(),
        })
    }

    fn home(&self) -> Home<'_, HomeAssistant> {
        Home {
            cache: self.home_assistant.cache(),
            api: &self.home_assistant,
            connected: self.home_assistant.status() == ConnectionStatus::Connected,
        }
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, CommandError> {
    Ok(state.settings()?)
}

/// Saves the form. A new access token is stored only in the OS credential store.
#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    settings: Settings,
    token: Option<String>,
) -> Result<Settings, CommandError> {
    let previous = state.settings()?;
    let settings = Settings {
        active_model: previous.active_model.clone(),
        ..settings.validated()?
    };
    let token = token.and_then(AccessToken::new);
    if let Some(token) = &token {
        credentials::save_home_assistant_token(token)?;
    }
    if settings.home_assistant_url.is_empty() {
        credentials::delete_home_assistant_token()?;
    }
    settings::save(&state.db(), &settings)?;

    if token.is_some() || settings.home_assistant_url != previous.home_assistant_url {
        state.connect_home_assistant(&settings.home_assistant_url);
    }
    if settings.context_length != previous.context_length {
        // The next request reloads the model with the new context length.
        let engine = state.engine.clone();
        tauri::async_runtime::spawn(async move { engine.unload().await });
    }
    Ok(settings)
}

#[tauri::command]
pub fn has_home_assistant_token() -> Result<bool, CommandError> {
    Ok(credentials::load_home_assistant_token()?.is_some())
}

#[tauri::command]
pub async fn discover_home_assistant() -> Result<Vec<DiscoveredInstance>, CommandError> {
    Ok(discovery::discover().await?)
}

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> Status {
    state.status()
}

#[tauri::command]
pub fn list_models(state: State<'_, AppState>) -> Result<Vec<ModelInfo>, CommandError> {
    let settings = state.settings()?;
    Ok(state
        .models
        .list(settings.active_model.as_deref(), settings.context_length))
}

#[tauri::command]
pub fn download_model(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    Ok(state.models.start_download(&id)?)
}

#[tauri::command]
pub fn pause_download(state: State<'_, AppState>, id: String) {
    state.models.pause_download(&id);
}

#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    Ok(state.models.cancel_download(&id)?)
}

#[tauri::command]
pub fn delete_model(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    let settings = state.settings()?;
    let in_use =
        settings.active_model.as_deref() == Some(id.as_str()) || state.engine.is_loaded(&id);
    Ok(state.models.delete(&id, in_use)?)
}

/// Makes an installed model active and loads it, replacing any loaded model.
#[tauri::command]
pub async fn select_model(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    state.models.installed(&id)?;
    let settings = Settings {
        active_model: Some(id),
        ..state.settings()?
    };
    settings::save(&state.db(), &settings)?;
    state.models.notify_changed();
    state.session.cancel();
    let spec = state.model_spec(&settings)?;
    Ok(state.engine.load(&spec).await?)
}

const HOME_ASSISTANT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Called when Luna's window is shown, so the model is ready before the user asks.
/// Failures are logged; the model status already tells the user if loading failed.
#[tauri::command]
pub async fn prepare_assistant(state: State<'_, AppState>) -> Result<(), CommandError> {
    let spec = match state
        .settings()
        .and_then(|settings| state.model_spec(&settings))
    {
        Ok(spec) => spec,
        Err(AppError::NoActiveModel) => return Ok(()),
        Err(error) => {
            log::warn!("cannot prepare the assistant: {error}");
            return Ok(());
        }
    };
    // The shared prompt holds the home layout, so it is prepared only once that is loaded.
    let mut status = state.home_assistant.subscribe_status();
    let settled = tokio::time::timeout(
        HOME_ASSISTANT_WAIT,
        status.wait_for(|status| {
            !matches!(
                status,
                ConnectionStatus::Connecting | ConnectionStatus::Reconnecting
            )
        }),
    )
    .await
    .ok()
    .and_then(|result| result.ok().map(|status| *status));
    if settled != Some(ConnectionStatus::Connected) {
        log::info!("assistant not prepared: Home Assistant is not connected");
        return Ok(());
    }
    let started = std::time::Instant::now();
    match assistant::warm_up(&state.engine, &spec, state.home()).await {
        Ok(Warmup::AlreadyReady) => {}
        Ok(warmup) => log::info!(
            "assistant ready in {} ms ({warmup:?})",
            started.elapsed().as_millis()
        ),
        Err(error) => log::error!("failed to prepare the assistant: {error}"),
    }
    Ok(())
}

#[tauri::command]
pub async fn ask(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
) -> Result<Interaction, CommandError> {
    end_conversation_when_idle(&app);
    let text = text.trim().to_owned();
    let settings = state.settings()?;
    let spec = state.model_spec(&settings)?;
    let history = history::recent(&state.db(), VISIBLE_HISTORY)?;
    let cancel = state.session.begin()?;
    let _finished = SessionGuard(&state.session);

    let model = EngineChat {
        engine: &state.engine,
        spec: &spec,
    };
    let mut memory = state.session.memory();
    let result =
        assistant::respond(&model, state.home(), &history, &mut memory, &text, &cancel).await;
    state.session.remember(memory);
    let reply = result?;
    log::info!("request handled: {}", reply.metrics);
    let interaction = history::insert(
        &state.db(),
        &text,
        &reply.text,
        &reply.results,
        !reply.confirmation.is_empty(),
    )?;
    if !reply.confirmation.is_empty() {
        state
            .session
            .await_confirmation(interaction.id, reply.confirmation);
    }
    end_conversation_when_idle(&app);
    Ok(interaction)
}

/// Clears the conversation once no request follows within `CONVERSATION_LIFETIME`.
fn end_conversation_when_idle(app: &AppHandle) {
    let activity = app.state::<AppState>().session.touch();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CONVERSATION_LIFETIME).await;
        let state = app.state::<AppState>();
        if !state.session.idle_since(activity) {
            return;
        }
        state.session.forget();
        match history::clear(&state.db()) {
            Ok(()) => crate::emit(&app, crate::CONVERSATION_EVENT, ()),
            Err(error) => log::error!("failed to clear the conversation: {error}"),
        }
    });
}

#[tauri::command]
pub fn cancel_request(state: State<'_, AppState>) {
    state.session.cancel();
}

#[tauri::command]
pub async fn confirm_action(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    confirmed: bool,
) -> Result<Interaction, CommandError> {
    end_conversation_when_idle(&app);
    let text = match (state.session.take_confirmation(id), confirmed) {
        (Ok(requests), true) => {
            let mut memory = state.session.memory();
            let reply = assistant::confirm(state.home(), &mut memory, &requests).await;
            state.session.remember(memory);
            log::info!("confirmation handled: {}", reply.metrics);
            reply.text
        }
        (Ok(_), false) => "Okay, nothing changed.".to_owned(),
        (Err(_), _) => "That request expired, so nothing changed.".to_owned(),
    };
    Ok(history::resolve(&state.db(), id, &text, &[])?)
}

#[tauri::command]
pub fn list_interactions(state: State<'_, AppState>) -> Result<Vec<Interaction>, CommandError> {
    Ok(history::recent(&state.db(), VISIBLE_HISTORY)?)
}

#[tauri::command]
pub fn clear_history(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.session.forget();
    Ok(history::clear(&state.db())?)
}

#[tauri::command]
pub fn get_launch_at_login(app: AppHandle) -> Result<bool, CommandError> {
    Ok(app
        .autolaunch()
        .is_enabled()
        .map_err(|error| AppError::LaunchAtLogin(error.to_string()))?)
}

/// Returns the state reported by the operating system after the change.
#[tauri::command]
pub fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<bool, CommandError> {
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result
        .and_then(|()| launcher.is_enabled())
        .map_err(|error| AppError::LaunchAtLogin(error.to_string()).into())
}

/// Marks the request finished even if the command future is dropped.
struct SessionGuard<'a>(&'a Session);

impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}
