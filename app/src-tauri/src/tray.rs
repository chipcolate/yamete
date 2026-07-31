//! The menu bar item.
//!
//! Left click toggles the panel; right click opens a menu with the things you'd want
//! without opening a window at all.

use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

use crate::daemon::Daemon;

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Spank…", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Pause detection", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Show logs…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Spank", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &toggle,
            &logs,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    TrayIconBuilder::with_id("spank")
        .icon(app.default_window_icon().cloned().ok_or_else(|| {
            tauri::Error::AssetNotFound("no default window icon configured".into())
        })?)
        // A template image is tinted by macOS to match the menu bar, so it stays legible
        // in both light and dark mode and while the bar is highlighted.
        .icon_as_template(true)
        .tooltip("Spank")
        .menu(&menu)
        // Left click should toggle the panel, so the menu is bound to right click only.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_window(app),
            "toggle" => toggle_detection(app),
            "logs" => {
                let _ = crate::open_logs(app.clone());
            }
            "quit" => {
                // Quitting the controller deliberately leaves the daemon running — the
                // detector is the product, this is just a window onto it.
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window("main")
}

fn show_window(app: &AppHandle) {
    if let Some(window) = main_window(app) {
        // An Accessory-policy app has to be explicitly activated or its window opens
        // behind whatever you were using.
        #[cfg(target_os = "macos")]
        let _ = app.show();
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(daemon) = app.try_state::<Arc<Daemon>>() {
            daemon.set_telemetry(true);
        }
    }
}

fn toggle_window(app: &AppHandle) {
    let Some(window) = main_window(app) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        if let Some(daemon) = app.try_state::<Arc<Daemon>>() {
            daemon.set_telemetry(false);
        }
    } else {
        show_window(app);
    }
}

fn toggle_detection(app: &AppHandle) {
    let Some(daemon) = app.try_state::<Arc<Daemon>>() else {
        return;
    };
    let currently_enabled = match daemon.request(&spank_proto::Request::GetConfig) {
        Ok(spank_proto::Event::Config { config }) => config.enabled,
        _ => return,
    };
    let _ = daemon.request(&spank_proto::Request::SetEnabled {
        value: !currently_enabled,
    });
}
