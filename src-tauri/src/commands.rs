use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::error::{AppError, CommandError};
use crate::settings::{self, Settings};

pub struct AppState {
    db: Mutex<Connection>,
}

impl AppState {
    pub fn new(db: Connection) -> Self {
        Self { db: Mutex::new(db) }
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite in a partial state.
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, CommandError> {
    Ok(settings::load(&state.db())?)
}

#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, CommandError> {
    let settings = settings.validated().map_err(AppError::from)?;
    settings::save(&state.db(), &settings)?;
    Ok(settings)
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
