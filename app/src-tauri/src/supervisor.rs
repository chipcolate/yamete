//! Owning the daemon's lifetime.
//!
//! The app bundles `spankd` as a sidecar and runs it as a child process, so opening Spank
//! starts detection and quitting Spank stops it. That is the opposite of the launchd
//! arrangement, where the daemon outlives the app — the tradeoff being that detection now
//! only happens while the app is open, which is what makes the menu bar icon mean
//! something.
//!
//! Two rules keep this honest:
//!
//! * **We only kill what we started.** If a daemon is already listening — someone has the
//!   LaunchAgent installed, or is running one from a terminal — we attach to it and leave
//!   it alone on exit. Killing a process we did not spawn would be rude and surprising.
//! * **The child cannot outlive us.** It is spawned with a stdin pipe and `--exit-with-
//!   parent`; the pipe closes when this process dies for any reason, including a force
//!   quit, so there is no path that leaves an orphaned daemon holding the sensor.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long to wait for a freshly spawned daemon to start listening.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The daemon process, if this app started one.
pub struct Supervisor {
    child: Mutex<Option<Child>>,
}

impl Supervisor {
    pub fn new() -> Self {
        Supervisor {
            child: Mutex::new(None),
        }
    }

    /// Whether a daemon is currently reachable.
    pub fn daemon_is_running() -> bool {
        UnixStream::connect(spank_proto::socket_path()).is_ok()
    }

    /// Path to the bundled `spankd`.
    ///
    /// Tauri strips the target-triple suffix and places sidecars next to the main
    /// executable, so this is simply a sibling. Falls back to a development build so
    /// `cargo run` works without bundling.
    fn sidecar_path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;

        let bundled = dir.join("spankd");
        if bundled.exists() {
            return Some(bundled);
        }
        // Running from a build directory: app/src-tauri/target/<profile>/spank-app
        for candidate in [
            dir.join("../../../../target/release/spankd"),
            dir.join("../../../../target/debug/spankd"),
        ] {
            if candidate.exists() {
                return candidate.canonicalize().ok();
            }
        }
        None
    }

    /// Start the daemon unless one is already running.
    ///
    /// Returns a human-readable note about what happened, for the log.
    pub fn ensure_running(&self) -> Result<String, String> {
        if Self::daemon_is_running() {
            return Ok("attached to a daemon that was already running".into());
        }

        let path = Self::sidecar_path()
            .ok_or_else(|| "could not find the bundled spankd binary".to_string())?;

        let child = Command::new(&path)
            .args(["run", "--daemon", "--exit-with-parent"])
            // The pipe is the leash: spankd exits when it closes.
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", path.display()))?;

        *self.child.lock().map_err(|e| e.to_string())? = Some(child);

        // Wait for it to bind, so the UI does not flash "not running" on every launch.
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while Instant::now() < deadline {
            if Self::daemon_is_running() {
                return Ok(format!("started {}", path.display()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err("spankd did not start listening within ten seconds".into())
    }

    /// Stop the daemon, but only if this app started it.
    pub fn shutdown(&self) {
        let Ok(mut guard) = self.child.lock() else {
            return;
        };
        let Some(mut child) = guard.take() else {
            // Nothing we started; anything running belongs to launchd or a terminal.
            return;
        };

        // Dropping stdin closes the pipe, which is the daemon's cue to shut down cleanly
        // and remove its socket. Only escalate if it ignores that.
        drop(child.stdin.take());

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Whether this app owns the running daemon.
    pub fn owns_daemon(&self) -> bool {
        self.child
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}
