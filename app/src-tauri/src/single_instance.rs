//! One controller at a time.
//!
//! Nothing stops a second copy of the app launching — a build in `target/`, a download in
//! `~/Downloads`, and the installed one in `/Applications` are three different bundles as
//! far as Launch Services is concerned. Two menu bar icons for the same thing is confusing,
//! and both would hold their own telemetry subscription.
//!
//! The daemon solves this by owning a socket; the same trick works here, with the addition
//! that a second launch hands the request over to the first instance rather than just
//! dying silently.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use tauri::{AppHandle, Manager};

/// Message a second instance sends to the first.
const SHOW: &str = "show";

fn socket_path() -> PathBuf {
    yamete_proto::state_dir().join("app.sock")
}

/// Outcome of the startup check.
pub enum Instance {
    /// We are the only one; hold this listener for the process lifetime.
    First(UnixListener),
    /// Another instance is running and has been asked to show itself.
    Second,
}

/// Determine whether this process should run, handing off to an existing instance if one
/// is already up.
pub fn acquire() -> std::io::Result<Instance> {
    let path = socket_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // A socket file is not proof of a live process — it survives a crash. Probing it is
    // the only way to tell "already running" from "left behind".
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(mut existing) => {
                let _ = writeln!(existing, "{SHOW}");
                return Ok(Instance::Second);
            }
            Err(_) => {
                std::fs::remove_file(&path)?;
            }
        }
    }

    let listener = UnixListener::bind(&path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(Instance::First(listener))
}

/// Listen for later launches and surface the window when one arrives.
pub fn spawn_listener(app: AppHandle, listener: UnixListener) {
    std::thread::Builder::new()
        .name("yamete-single-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                if line.trim() == SHOW {
                    show(&app);
                }
            }
        })
        .expect("could not spawn the single-instance listener");
}

fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // An Accessory-policy app needs explicit activation or the window appears behind
        // whatever the user was doing.
        #[cfg(target_os = "macos")]
        let _ = app.show();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Remove the socket on a clean exit so the next launch does not have to reclaim it.
pub fn release() {
    let _ = std::fs::remove_file(socket_path());
}
