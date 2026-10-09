mod actions;
mod assistant;
mod commands;
mod credentials;
mod db;
mod error;
mod history;
mod home_assistant;
mod inference;
mod lifecycle;
mod models;
mod settings;
mod tls;

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::MacosLauncher;

use commands::AppState;
use inference::Engine;
use models::{DownloadProgress, ModelEvents, ModelManager, ModelStore};

const STATUS_EVENT: &str = "status-changed";
const MODELS_EVENT: &str = "models-changed";
const PROGRESS_EVENT: &str = "download-progress";

struct FrontendEvents(AppHandle);

impl ModelEvents for FrontendEvents {
    fn changed(&self) {
        emit(&self.0, MODELS_EVENT, ());
    }

    fn progress(&self, progress: DownloadProgress) {
        emit(&self.0, PROGRESS_EVENT, progress);
    }
}

fn emit(app: &AppHandle, event: &str, payload: impl serde::Serialize + Clone) {
    if let Err(error) = app.emit(event, payload) {
        log::error!("failed to emit {event}: {error}");
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        // Must be registered first so a second launch focuses the running instance.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            lifecycle::show_main_window(app);
        }))
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![lifecycle::START_HIDDEN_ARG]),
        ))
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = db::open(&data_dir.join("luna.db")).inspect_err(|error| {
                log::error!("failed to open database: {error}");
            })?;
            let store = ModelStore::open(data_dir.join("models"))?;
            let events = Arc::new(FrontendEvents(app.handle().clone()));
            let models = Arc::new(ModelManager::new(
                models::catalog::models().to_vec(),
                store,
                events,
            )?);
            let engine = Engine::new(
                Engine::bundled_runtime()?,
                data_dir.join("llama-server.pid"),
            );
            let state = AppState::new(db, engine, models);
            state.connect_home_assistant(&state.settings()?.home_assistant_url);
            app.manage(state);
            forward_status(app.handle());
            exit_on_terminate(app.handle());

            lifecycle::setup_tray(app)?;
            if !lifecycle::starts_hidden(std::env::args()) {
                lifecycle::show_main_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(lifecycle::handle_window_event)
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::has_home_assistant_token,
            commands::discover_home_assistant,
            commands::get_status,
            commands::list_models,
            commands::download_model,
            commands::pause_download,
            commands::cancel_download,
            commands::delete_model,
            commands::select_model,
            commands::ask,
            commands::cancel_request,
            commands::confirm_action,
            commands::list_interactions,
            commands::clear_history,
            commands::get_launch_at_login,
            commands::set_launch_at_login,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Luna");

    app.run(|app, event| match event {
        // Never leave a model loaded in a background process after Luna quits.
        tauri::RunEvent::Exit => {
            let engine = app.state::<AppState>().engine.clone();
            tauri::async_runtime::block_on(engine.unload());
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => lifecycle::show_main_window(app),
        _ => {}
    });
}

/// Logout and shutdown send SIGTERM on macOS. Quitting cleanly unloads the model.
#[cfg(unix)]
fn exit_on_terminate(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
                app.exit(0);
            }
            Err(error) => log::error!("failed to listen for termination: {error}"),
        }
    });
}

#[cfg(not(unix))]
fn exit_on_terminate(_: &AppHandle) {}

/// Sends connection and model status changes to the frontend as they happen.
fn forward_status(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut home_assistant = state.home_assistant.subscribe_status();
    let mut engine = state.engine.subscribe_status();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                changed = home_assistant.changed() => if changed.is_err() { return },
                changed = engine.changed() => if changed.is_err() { return },
            }
            emit(&app, STATUS_EVENT, app.state::<AppState>().status());
        }
    });
}
