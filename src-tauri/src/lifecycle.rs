use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, Window, WindowEvent, Wry};

use crate::commands::AppState;

/// Passed by the login item so Luna starts in the tray without opening its window.
pub const START_HIDDEN_ARG: &str = "--hidden";

const MAIN_WINDOW: &str = "main";
const MENU_OPEN: &str = "open";
const MENU_LISTENING: &str = "listening";
const MENU_QUIT: &str = "quit";

pub fn starts_hidden<I: IntoIterator<Item = String>>(args: I) -> bool {
    args.into_iter().any(|arg| arg == START_HIDDEN_ARG)
}

pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        log::error!("main window is missing");
        return;
    };
    let result = window
        .show()
        .and_then(|()| window.unminimize())
        .and_then(|()| window.set_focus());
    if let Err(error) = result {
        log::error!("failed to show main window: {error}");
    }
    app.state::<AppState>().set_window_visible(true);
}

/// Closing the window keeps Luna running in the tray. Quit is in the tray menu.
pub fn handle_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        if let Err(error) = window.hide() {
            log::error!("failed to hide main window: {error}");
        }
        window.state::<AppState>().set_window_visible(false);
    }
}

/// The tray's listening item, kept so it can follow changes made in the window.
struct ListeningItem(CheckMenuItem<Wry>);

pub fn setup_tray(app: &App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, MENU_OPEN, "Open Luna", true, None::<&str>)?;
    let listening =
        CheckMenuItem::with_id(app, MENU_LISTENING, "Listening", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit Luna", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &listening, &separator, &quit])?;
    app.manage(ListeningItem(listening));

    TrayIconBuilder::with_id("luna")
        .icon(tray_icon()?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Luna")
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN => show_main_window(app),
            MENU_LISTENING => {
                let enabled = app.state::<ListeningItem>().0.is_checked().unwrap_or(false);
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let result = crate::listening::set_enabled_in_background(app.clone(), enabled);
                    if result.await.is_err() {
                        show_main_window(&app);
                    }
                });
            }
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Keeps the tray's listening check mark in step with the actual state. Menus change on the
/// main thread; this never waits for it, so the voice thread cannot block on it.
pub fn show_listening(app: &AppHandle, on: bool) {
    let handle = app.clone();
    let result = app.run_on_main_thread(move || {
        if let Some(item) = handle.try_state::<ListeningItem>()
            && let Err(error) = item.0.set_checked(on)
        {
            log::error!("failed to update the tray menu: {error}");
        }
    });
    if let Err(error) = result {
        log::error!("failed to update the tray menu: {error}");
    }
}

fn tray_icon() -> tauri::Result<Image<'static>> {
    // macOS menu bar icons are monochrome templates; Windows uses the full-colour icon.
    #[cfg(target_os = "macos")]
    let bytes = include_bytes!("../icons/tray-template.png").as_slice();
    #[cfg(not(target_os = "macos"))]
    let bytes = include_bytes!("../icons/32x32.png").as_slice();
    Image::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn starts_visible_by_default() {
        assert!(!starts_hidden(args(&["luna"])));
    }

    #[test]
    fn starts_hidden_when_launched_at_login() {
        assert!(starts_hidden(args(&["luna", START_HIDDEN_ARG])));
    }

    #[test]
    fn ignores_similar_arguments() {
        assert!(!starts_hidden(args(&["luna", "--hidden=false", "hidden"])));
    }
}
