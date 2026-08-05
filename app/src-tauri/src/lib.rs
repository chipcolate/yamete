//! Yamete — a menu bar controller for the slap detector.
//!
//! Deliberately thin. The daemon does the work and keeps running whether or not this is
//! open; the app exists to show you what the detector is seeing and to change its mind
//! about what counts as a slap.

mod daemon;
mod single_instance;
mod supervisor;
mod tray;

use std::sync::Arc;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Hand off to an existing instance before doing any setup — otherwise a second launch
    // would briefly add a tray icon and a telemetry subscription before noticing.
    let mut listener = match single_instance::acquire() {
        Ok(single_instance::Instance::First(listener)) => Some(listener),
        Ok(single_instance::Instance::Second) => {
            // The running instance has been asked to show itself; nothing left to do.
            return;
        }
        // A failure here should not stop the app starting; worst case is the old
        // behaviour of allowing two.
        Err(e) => {
            eprintln!("[yamete-app] single-instance check failed: {e}");
            None
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(daemon::Daemon::new()))
        .manage(Arc::new(supervisor::Supervisor::new()))
        .invoke_handler(tauri::generate_handler![
            daemon_owned,
            daemon::get_config,
            daemon::set_config,
            daemon::set_enabled,
            daemon::get_status,
            daemon::test_action,
            daemon::is_connected,
            daemon::set_scope_visible,
            quit_app,
            open_logs,
        ])
        .setup(move |app| {
            // A menu bar utility, not an app you switch to. `Accessory` removes the Dock
            // icon and the Cmd-Tab entry; `LSUIElement` in Info.plist does the same thing
            // earlier, before the process has a chance to bounce in the Dock.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Start the detector before anything else — the app exists to control it, so
            // opening the app should mean detection is running.
            let supervisor = app.state::<Arc<supervisor::Supervisor>>().inner().clone();
            match supervisor.ensure_running() {
                Ok(note) => eprintln!("[yamete-app] {note}"),
                Err(e) => eprintln!("[yamete-app] could not start the daemon: {e}"),
            }

            tray::build(app.handle())?;

            if let Some(listener) = listener.take() {
                single_instance::spawn_listener(app.handle().clone(), listener);
            }

            let state = app.state::<Arc<daemon::Daemon>>().inner().clone();
            daemon::spawn_subscription(app.handle().clone(), state.clone());
            daemon::spawn_visibility_watch(app.handle().clone(), state);

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Closing the window must not quit: the tray icon is the app, and the
                // daemon keeps detecting regardless. Hide instead.
                let _ = window.hide();
                api.prevent_close();
                stop_telemetry(window.app_handle());
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building spank")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Quitting the app stops detection: the daemon is ours, so it goes too.
                if let Some(supervisor) = app.try_state::<Arc<supervisor::Supervisor>>() {
                    supervisor.shutdown();
                }
                single_instance::release();
            }
        });
}

/// Telemetry is the only thing that costs the daemon anything, so it is switched off the
/// moment nothing is displaying it.
fn stop_telemetry(app: &tauri::AppHandle) {
    if let Some(daemon) = app.try_state::<Arc<daemon::Daemon>>() {
        daemon.set_telemetry(false);
    }
}

/// Quit from the UI, which shuts the daemon down on the way out.
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Whether this app started the daemon, as opposed to attaching to an existing one.
///
/// Surfaced so the UI can be honest about what Quit will do.
#[tauri::command]
fn daemon_owned(supervisor: tauri::State<'_, Arc<supervisor::Supervisor>>) -> bool {
    supervisor.owns_daemon()
}

/// Reveal the daemon's log directory in Finder, for when something is wrong.
#[tauri::command]
fn open_logs(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Library/Logs/yamete"))
        .map_err(|e| e.to_string())?;
    // Created on demand as well as at daemon start, so this can never open nothing —
    // an empty folder at least says where the logs would be.
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}
