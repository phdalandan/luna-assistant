use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::assistant::{self, CONVERSATION_LIFETIME, Home, Provider, Session};
use crate::credentials::{self, AccessToken};
use crate::error::{AppError, CommandError};
use crate::history::{self, Interaction};
use crate::home_assistant::discovery::{self, DiscoveredInstance};
use crate::home_assistant::{ConnectionStatus, HomeAssistant};
use crate::inference::{CloudClient, Engine, EngineStatus, ModelSpec, Warmup};
use crate::listening::{self, VoiceStatus};
use crate::models::{CloudProvider, ModelInfo, ModelManager, VoiceModelsInfo, catalog};
use crate::settings::{self, InferenceMode, Settings};
use crate::voice::Voice;

const VISIBLE_HISTORY: usize = 30;

pub struct AppState {
    db: Mutex<Connection>,
    pub home_assistant: HomeAssistant,
    pub engine: Engine,
    pub models: Arc<ModelManager>,
    cloud: CloudClient,
    session: Session,
    pub voice: Voice,
    voice_status: Mutex<VoiceStatus>,
    /// When the conversation will be cleared if nothing else is asked, in Unix milliseconds.
    conversation_ends_at: Mutex<Option<i64>>,
    /// Whether Luna's window is showing, so nothing is sent to it while hidden.
    window_visible: AtomicBool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub home_assistant: ConnectionStatus,
    pub engine: EngineStatus,
    pub inference: InferenceMode,
    pub voice: VoiceStatus,
    /// When the conversation resets, in Unix milliseconds, or `None` when there is none.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub conversation_ends_at: Option<i64>,
}

impl AppState {
    pub fn new(
        db: Connection,
        engine: Engine,
        models: Arc<ModelManager>,
        cloud: CloudClient,
    ) -> Self {
        Self {
            db: Mutex::new(db),
            home_assistant: HomeAssistant::default(),
            engine,
            models,
            cloud,
            session: Session::default(),
            voice: Voice::default(),
            voice_status: Mutex::default(),
            conversation_ends_at: Mutex::default(),
            window_visible: AtomicBool::new(false),
        }
    }

    pub fn db(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite in a partial state.
        lock(&self.db)
    }

    pub fn settings(&self) -> Result<Settings, AppError> {
        settings::load(&self.db())
    }

    pub fn status(&self) -> Status {
        Status {
            home_assistant: self.home_assistant.status(),
            engine: self.engine.status(),
            inference: self
                .settings()
                .map(|settings| settings.inference)
                .unwrap_or_else(|error| {
                    log::error!("cannot read the inference mode: {error}");
                    InferenceMode::default()
                }),
            voice: lock(&self.voice_status).clone(),
            conversation_ends_at: *lock(&self.conversation_ends_at),
        }
    }

    /// Returns whether the status changed.
    pub fn set_voice_status(&self, status: VoiceStatus) -> bool {
        let mut current = lock(&self.voice_status);
        let changed = *current != status;
        *current = status;
        changed
    }

    pub fn window_visible(&self) -> bool {
        self.window_visible.load(Ordering::Relaxed)
    }

    pub fn set_window_visible(&self, visible: bool) {
        self.window_visible.store(visible, Ordering::Relaxed);
    }

    pub fn pending_confirmation(&self) -> Option<i64> {
        self.session.pending_confirmation()
    }

    pub fn cancel_request(&self) {
        self.session.cancel();
    }

    /// Handles one request, typed or spoken, and records it in the conversation.
    pub async fn ask(&self, app: &AppHandle, text: &str) -> Result<Interaction, AppError> {
        end_conversation_when_idle(app);
        let text = text.trim();
        let settings = self.settings()?;
        let model = self.provider(&settings)?;
        let history = history::recent(&self.db(), VISIBLE_HISTORY)?;
        let cancel = self.session.begin()?;
        let _finished = SessionGuard(&self.session);

        let mut memory = self.session.memory();
        let result =
            assistant::respond(&model, self.home(), &history, &mut memory, text, &cancel).await;
        self.session.remember(memory);
        let reply = result?;
        log::info!("request handled: {}", reply.metrics);
        let interaction = history::insert(
            &self.db(),
            text,
            &reply.text,
            &reply.results,
            !reply.confirmation.is_empty(),
        )?;
        if !reply.confirmation.is_empty() {
            self.session
                .await_confirmation(interaction.id, reply.confirmation);
        }
        end_conversation_when_idle(app);
        Ok(interaction)
    }

    pub async fn confirm(
        &self,
        app: &AppHandle,
        id: i64,
        confirmed: bool,
    ) -> Result<Interaction, AppError> {
        end_conversation_when_idle(app);
        let text = match (self.session.take_confirmation(id), confirmed) {
            (Ok(requests), true) => {
                let mut memory = self.session.memory();
                let reply = assistant::confirm(self.home(), &mut memory, &requests).await;
                self.session.remember(memory);
                log::info!("confirmation handled: {}", reply.metrics);
                reply.text
            }
            (Ok(_), false) => "Okay, nothing changed.".to_owned(),
            (Err(_), _) => "That request expired, so nothing changed.".to_owned(),
        };
        history::resolve(&self.db(), id, &text, &[])
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

    /// Saves where inference runs. A request in progress stops before its next model pass;
    /// actions being executed are never interrupted. Returns whether the mode changed.
    pub fn save_inference_mode(&self, mode: InferenceMode) -> Result<bool, AppError> {
        let current = self.settings()?;
        if current.inference == mode {
            return Ok(false);
        }
        let settings = Settings {
            inference: mode,
            ..current
        };
        settings::save(&self.db(), &settings)?;
        self.session.cancel();
        Ok(true)
    }

    /// Releases the local model in Cloud mode, or loads the selected one in Local mode.
    pub async fn match_engine_to_mode(&self) -> Result<(), AppError> {
        let settings = self.settings()?;
        match settings.inference {
            InferenceMode::Cloud => self.engine.unload().await,
            InferenceMode::Local if settings.active_model.is_some() => {
                self.engine.load(&self.model_spec(&settings)?).await?;
            }
            InferenceMode::Local => {}
        }
        Ok(())
    }

    /// The provider for the saved choice. A missing key or model is an error, never a fallback.
    fn provider(&self, settings: &Settings) -> Result<Provider<'_>, AppError> {
        if settings.inference == InferenceMode::Local {
            return Ok(Provider::Local {
                engine: &self.engine,
                spec: self.model_spec(settings)?,
            });
        }
        let provider = settings.cloud_provider;
        let id = settings.cloud_model(provider);
        let model = catalog::cloud_model(provider, id)
            .ok_or_else(|| AppError::UnknownCloudModel(provider, id.to_owned()))?;
        let key = credentials::load_api_key(provider)?.ok_or(AppError::NoApiKey(provider))?;
        Ok(Provider::Cloud {
            client: &self.cloud,
            model,
            key,
        })
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

/// Saves the form. A new access token or API key is stored only in the OS credential store.
#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
    token: Option<String>,
    api_key: Option<String>,
) -> Result<Settings, CommandError> {
    let previous = state.settings()?;
    let settings = Settings::from_form(settings, &previous)?;
    let token = token.and_then(AccessToken::new);
    if let Some(token) = &token {
        credentials::save_home_assistant_token(token)?;
    }
    if let Some(key) = api_key.and_then(AccessToken::new) {
        credentials::save_api_key(settings.cloud_provider, &key)?;
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
    let voice_changed =
        settings.wake_word != previous.wake_word || settings.voice != previous.voice;
    if voice_changed && settings.listening {
        // Listening restarts with the new wake word or voice, away from the main thread.
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(error) = listening::start(&app) {
                log::warn!("listening did not restart with the new wake word: {error}");
            }
        });
    }
    Ok(settings)
}

#[tauri::command]
pub fn has_home_assistant_token() -> Result<bool, CommandError> {
    Ok(credentials::load_home_assistant_token()?.is_some())
}

/// Which providers have a saved API key. Keys themselves never leave Rust.
#[tauri::command]
pub fn saved_api_keys() -> Result<Vec<CloudProvider>, CommandError> {
    let mut saved = Vec::new();
    for provider in CloudProvider::ALL {
        if credentials::load_api_key(provider)?.is_some() {
            saved.push(provider);
        }
    }
    Ok(saved)
}

#[tauri::command]
pub fn remove_api_key(provider: CloudProvider) -> Result<(), CommandError> {
    Ok(credentials::delete_api_key(provider)?)
}

/// A cloud model for the choice in Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct CloudModelOption {
    pub provider: CloudProvider,
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub fn list_cloud_models() -> Vec<CloudModelOption> {
    catalog::cloud_models()
        .iter()
        .map(|model| CloudModelOption {
            provider: model.provider,
            id: model.id.clone(),
            name: model.name.clone(),
        })
        .collect()
}

/// Switches between Local and Cloud without restarting Luna or clearing the conversation.
#[tauri::command]
pub async fn set_inference_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: InferenceMode,
) -> Result<(), CommandError> {
    if state.save_inference_mode(mode)? {
        crate::emit(&app, crate::STATUS_EVENT, state.status());
        state.match_engine_to_mode().await?;
    }
    Ok(())
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
    if settings.inference == InferenceMode::Cloud {
        return Ok(());
    }
    state.session.cancel();
    let spec = state.model_spec(&settings)?;
    Ok(state.engine.load(&spec).await?)
}

const HOME_ASSISTANT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Called when Luna's window is shown, so the model is ready before the user asks.
/// Failures are logged; the model status already tells the user if loading failed.
#[tauri::command]
pub async fn prepare_assistant(state: State<'_, AppState>) -> Result<(), CommandError> {
    let spec = match state.settings().and_then(|settings| {
        if settings.inference == InferenceMode::Cloud {
            return Err(AppError::NoActiveModel);
        }
        state.model_spec(&settings)
    }) {
        Ok(spec) => spec,
        // Nothing to load: no model is chosen, or a cloud provider answers instead.
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
    Ok(state.ask(&app, &text).await?)
}

/// Clears the conversation once no request follows within `CONVERSATION_LIFETIME`.
fn end_conversation_when_idle(app: &AppHandle) {
    let activity = app.state::<AppState>().session.touch();
    let ends_at = history::now_millis() + CONVERSATION_LIFETIME.as_millis() as i64;
    set_conversation_end(app, Some(ends_at));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CONVERSATION_LIFETIME).await;
        let state = app.state::<AppState>();
        if !state.session.idle_since(activity) {
            return;
        }
        state.session.forget();
        set_conversation_end(&app, None);
        match history::clear(&state.db()) {
            Ok(()) => crate::emit(&app, crate::CONVERSATION_EVENT, ()),
            Err(error) => log::error!("failed to clear the conversation: {error}"),
        }
    });
}

fn set_conversation_end(app: &AppHandle, ends_at: Option<i64>) {
    let state = app.state::<AppState>();
    *lock(&state.conversation_ends_at) = ends_at;
    crate::emit(app, crate::STATUS_EVENT, state.status());
}

#[tauri::command]
pub fn cancel_request(state: State<'_, AppState>) {
    state.cancel_request();
}

#[tauri::command]
pub async fn confirm_action(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    confirmed: bool,
) -> Result<Interaction, CommandError> {
    Ok(state.confirm(&app, id, confirmed).await?)
}

#[tauri::command]
pub fn list_interactions(state: State<'_, AppState>) -> Result<Vec<Interaction>, CommandError> {
    Ok(history::recent(&state.db(), VISIBLE_HISTORY)?)
}

#[tauri::command]
pub fn clear_history(app: AppHandle, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.session.forget();
    set_conversation_end(&app, None);
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

#[tauri::command]
pub async fn set_listening(app: AppHandle, enabled: bool) -> Result<(), CommandError> {
    Ok(listening::set_enabled_in_background(app, enabled).await?)
}

/// A voice Luna can speak with, for the choice in Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct VoiceOption {
    pub id: String,
    pub name: String,
    pub accent: String,
}

#[tauri::command]
pub fn list_voices() -> Vec<VoiceOption> {
    crate::models::catalog::speech()
        .speech_output
        .voices
        .iter()
        .map(|voice| VoiceOption {
            id: voice.id.clone(),
            name: voice.name.clone(),
            accent: voice.accent.clone(),
        })
        .collect()
}

#[tauri::command]
pub fn get_voice_models(state: State<'_, AppState>) -> VoiceModelsInfo {
    state.models.voice().info()
}

#[tauri::command]
pub fn download_voice_models(state: State<'_, AppState>) {
    state.models.voice().start_download();
}

#[tauri::command]
pub fn cancel_voice_download(state: State<'_, AppState>) {
    state.models.voice().cancel_download();
}

/// Marks the request finished even if the command future is dropped.
struct SessionGuard<'a>(&'a Session);

impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::inference::{DEFAULT_TIMEOUT, Endpoints};
    use crate::models::{DownloadProgress, ModelError, ModelEvents, ModelStore};

    struct NoEvents;

    impl ModelEvents for NoEvents {
        fn changed(&self) {}
        fn progress(&self, _: DownloadProgress) {}
    }

    fn state(name: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("luna-{name}-{}", std::process::id()));
        let store = ModelStore::open(dir.join("models")).unwrap();
        let models =
            ModelManager::new(catalog::models().to_vec(), store, Arc::new(NoEvents)).unwrap();
        let engine = Engine::new(
            PathBuf::from("/nonexistent/llama-server"),
            dir.join("llama-server.pid"),
            dir.join("prompt-cache"),
        );
        let db = crate::db::open_in_memory().unwrap();
        let cloud = CloudClient::new(Endpoints::default(), DEFAULT_TIMEOUT).unwrap();
        AppState::new(db, engine, Arc::new(models), cloud)
    }

    #[tokio::test]
    async fn switching_to_cloud_stops_the_request_and_releases_the_local_model() {
        let state = state("to-cloud");
        let request = state.session.begin().unwrap();

        assert!(state.save_inference_mode(InferenceMode::Cloud).unwrap());
        state.match_engine_to_mode().await.unwrap();

        assert!(request.is_cancelled());
        assert_eq!(state.settings().unwrap().inference, InferenceMode::Cloud);
        assert_eq!(state.status().inference, InferenceMode::Cloud);
        assert_eq!(state.engine.status(), EngineStatus::Idle);
        assert!(!state.save_inference_mode(InferenceMode::Cloud).unwrap());
    }

    #[tokio::test]
    async fn switching_to_local_loads_the_selected_model() {
        let state = state("to-local");
        state.save_inference_mode(InferenceMode::Cloud).unwrap();

        state.save_inference_mode(InferenceMode::Local).unwrap();
        state.match_engine_to_mode().await.unwrap();
        assert_eq!(state.engine.status(), EngineStatus::Idle);

        let settings = Settings {
            active_model: Some("qwen3-8b".into()),
            ..state.settings().unwrap()
        };
        settings::save(&state.db(), &settings).unwrap();
        let result = state.match_engine_to_mode().await;
        assert!(matches!(
            result,
            Err(AppError::Model(ModelError::NotInstalled(id))) if id == "qwen3-8b"
        ));
    }

    #[tokio::test]
    async fn cloud_mode_never_falls_back_to_the_local_model() {
        let state = state("no-fallback");
        let settings = Settings {
            inference: InferenceMode::Cloud,
            anthropic_model: "retired-model".into(),
            cloud_provider: CloudProvider::Anthropic,
            active_model: Some("qwen3-8b".into()),
            ..Settings::default()
        };
        let result = state.provider(&settings);
        assert!(matches!(
            result,
            Err(AppError::UnknownCloudModel(CloudProvider::Anthropic, _))
        ));
    }
}
