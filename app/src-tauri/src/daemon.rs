//! The app's connection to `spankd`.
//!
//! The app is a *controller*, not the detector — everything real happens in the daemon,
//! which runs under launchd whether or not this window is open. So this is a client that
//! attaches when it can, reconnects when it can't, and never assumes the daemon is there.
//!
//! Two channels run over the one socket: request/reply for config and status, and a
//! subscription stream for slaps and telemetry. Telemetry is only requested while a window
//! is actually visible, because it is the only thing that costs the daemon anything.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spank_proto::{DaemonConfig, Event, Request, Status};
use tauri::{AppHandle, Emitter, Manager};

/// How long to wait for a reply before giving up on a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// How long to wait before retrying a failed connection.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Shared connection state.
pub struct Daemon {
    /// Whether the subscription stream is currently connected.
    connected: Arc<AtomicBool>,
    /// Whether anyone wants telemetry right now.
    want_telemetry: Arc<AtomicBool>,
    /// Serialises request/reply exchanges, which use their own short-lived connection.
    request_lock: Mutex<()>,
}

impl Daemon {
    pub fn new() -> Self {
        Daemon {
            connected: Arc::new(AtomicBool::new(false)),
            want_telemetry: Arc::new(AtomicBool::new(false)),
            request_lock: Mutex::new(()),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Ask the daemon to start or stop streaming telemetry to us.
    pub fn set_telemetry(&self, want: bool) {
        self.want_telemetry.store(want, Ordering::Relaxed);
    }

    /// Send one request and wait for one reply.
    ///
    /// Uses a fresh connection rather than sharing the subscription socket: replies and
    /// streamed events are interleaved on a subscribed connection, so picking the reply
    /// out would mean buffering events. A short-lived socket is far simpler and this
    /// happens only on user actions.
    pub fn request(&self, request: &Request) -> Result<Event, String> {
        let _guard = self.request_lock.lock().map_err(|e| e.to_string())?;

        let mut stream = UnixStream::connect(spank_proto::socket_path()).map_err(|e| {
            format!("spankd is not running ({e}). Start it with `spankd install --copy`.")
        })?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(spank_proto::to_line(request).as_bytes())
            .map_err(|e| format!("could not send request: {e}"))?;

        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .map_err(|e| format!("no reply from spankd: {e}"))?;

        serde_json::from_str(&line).map_err(|e| format!("could not parse reply: {e}"))
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

/// Keep the telemetry subscription in step with whether the window is actually on screen.
///
/// This was previously driven from the webview via `document.visibilityState`, which is
/// the wrong source of truth: WKWebView reports its own occlusion, not whether the native
/// window is visible, and it does not reliably fire `visibilitychange` when the window is
/// shown or hidden natively. The result was a scope that stayed blank unless the window
/// happened to be opened via the tray. The window system is the authority, so ask it.
pub fn spawn_visibility_watch(app: AppHandle, daemon: Arc<Daemon>) {
    std::thread::Builder::new()
        .name("spank-visibility".into())
        .spawn(move || loop {
            let visible = app
                .get_webview_window("main")
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false);
            daemon.set_telemetry(visible);
            std::thread::sleep(Duration::from_millis(400));
        })
        .expect("could not spawn the visibility watcher");
}

/// Run the subscription loop until the app exits.
///
/// Every event is forwarded to the webview under its own name, so the frontend can listen
/// selectively rather than filtering a firehose.
pub fn spawn_subscription(app: AppHandle, daemon: Arc<Daemon>) {
    std::thread::Builder::new()
        .name("spank-subscription".into())
        .spawn(move || loop {
            match connect_and_stream(&app, &daemon) {
                Ok(()) => {}
                Err(e) => tracing_lite(&format!("subscription ended: {e}")),
            }
            daemon.connected.store(false, Ordering::Relaxed);
            let _ = app.emit("daemon-connection", false);
            std::thread::sleep(RECONNECT_DELAY);
        })
        .expect("could not spawn the subscription thread");
}

fn connect_and_stream(app: &AppHandle, daemon: &Arc<Daemon>) -> Result<(), String> {
    let stream = UnixStream::connect(spank_proto::socket_path())
        .map_err(|e| format!("could not connect: {e}"))?;
    let mut write_half = stream.try_clone().map_err(|e| e.to_string())?;

    daemon.connected.store(true, Ordering::Relaxed);
    let _ = app.emit("daemon-connection", true);

    let mut subscribed_telemetry = daemon.want_telemetry.load(Ordering::Relaxed);
    write_half
        .write_all(
            spank_proto::to_line(&Request::Subscribe {
                slaps: true,
                telemetry: subscribed_telemetry,
            })
            .as_bytes(),
        )
        .map_err(|e| e.to_string())?;

    // A read timeout doubles as the tick for noticing that the telemetry subscription
    // needs changing, without needing a second thread or a channel.
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    // Held across iterations on purpose. The read timeout above is what lets this loop
    // notice a changed telemetry subscription, but it also means `read_line` can return
    // mid-frame with bytes already consumed from the socket. Clearing the buffer at the
    // top of the loop would throw those away and leave the rest of the frame to be parsed
    // as a truncated line — which silently shredded exactly the large, frequent telemetry
    // frames while letting small, rare slap events through.
    let mut line = String::new();

    loop {
        let wanted = daemon.want_telemetry.load(Ordering::Relaxed);
        if wanted != subscribed_telemetry {
            subscribed_telemetry = wanted;
            write_half
                .write_all(
                    spank_proto::to_line(&Request::Subscribe {
                        slaps: true,
                        telemetry: wanted,
                    })
                    .as_bytes(),
                )
                .map_err(|e| e.to_string())?;
        }

        match reader.read_line(&mut line) {
            Ok(0) => return Err("daemon closed the connection".into()),
            Ok(_) => {
                if let Ok(event) = serde_json::from_str::<Event>(line.trim_end()) {
                    forward(app, event);
                }
                line.clear();
            }
            // Partial read: keep what we have and resume appending to it.
            Err(e) if is_timeout(&e) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn forward(app: &AppHandle, event: Event) {
    let _ = match event {
        Event::Slap(slap) => app.emit("slap", slap),
        Event::Telemetry(t) => app.emit("telemetry", t),
        Event::Config { config } => app.emit("config", config),
        Event::Status(s) => app.emit("status", s),
        Event::Error { message } => app.emit("daemon-error", message),
        Event::Pong => Ok(()),
    };
}

/// Minimal stderr logging. The app has no logging framework and does not need one.
fn tracing_lite(message: &str) {
    eprintln!("[spank-app] {message}");
}

// --- Commands callable from the webview ---

#[tauri::command]
pub fn get_config(daemon: tauri::State<'_, Arc<Daemon>>) -> Result<DaemonConfig, String> {
    match daemon.request(&Request::GetConfig)? {
        Event::Config { config } => Ok(*config),
        Event::Error { message } => Err(message),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn set_config(
    daemon: tauri::State<'_, Arc<Daemon>>,
    config: DaemonConfig,
) -> Result<DaemonConfig, String> {
    match daemon.request(&Request::SetConfig {
        config: Box::new(config),
    })? {
        Event::Config { config } => Ok(*config),
        Event::Error { message } => Err(message),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn set_enabled(
    daemon: tauri::State<'_, Arc<Daemon>>,
    value: bool,
) -> Result<DaemonConfig, String> {
    match daemon.request(&Request::SetEnabled { value })? {
        Event::Config { config } => Ok(*config),
        Event::Error { message } => Err(message),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn get_status(daemon: tauri::State<'_, Arc<Daemon>>) -> Result<Status, String> {
    match daemon.request(&Request::GetStatus)? {
        Event::Status(s) => Ok(s),
        Event::Error { message } => Err(message),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn test_action(
    daemon: tauri::State<'_, Arc<Daemon>>,
    id: String,
    intensity: f32,
) -> Result<(), String> {
    // A successful test produces no reply, so a timeout here means success. Anything that
    // does come back is a complaint worth surfacing.
    match daemon.request(&Request::TestAction { id, intensity }) {
        Ok(Event::Error { message }) => Err(message),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Whether the subscription stream is live, for the connection indicator.
#[tauri::command]
pub fn is_connected(daemon: tauri::State<'_, Arc<Daemon>>) -> bool {
    daemon.is_connected()
}

/// Start or stop telemetry, driven by whether the scope is on screen.
#[tauri::command]
pub fn set_telemetry(daemon: tauri::State<'_, Arc<Daemon>>, enabled: bool) {
    daemon.set_telemetry(enabled);
}
