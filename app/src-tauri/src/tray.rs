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
    let open = MenuItem::with_id(app, "open", "Open Yamete…", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Pause detection", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Show logs…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Yamete", true, None::<&str>)?;

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

    // Deliberately not the app icon. A template image is drawn from its alpha channel
    // alone, so full-colour artwork on an opaque background renders as a featureless blob
    // in the menu bar. This is a separate monochrome glyph, which is what lets macOS tint
    // it correctly in light mode, dark mode, and while the bar is highlighted.
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray@2x.png"))?;

    TrayIconBuilder::with_id("yamete")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Yamete")
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
    let currently_enabled = match daemon.request(&yamete_proto::Request::GetConfig) {
        Ok(yamete_proto::Event::Config { config }) => config.enabled,
        _ => return,
    };
    let _ = daemon.request(&yamete_proto::Request::SetEnabled {
        value: !currently_enabled,
    });
}
