//! Wire types shared between the daemon and anything that controls it.
//!
//! The protocol is newline-delimited JSON over a unix domain socket: one object per line,
//! bidirectional, no framing beyond the newline. That choice is deliberate — it means the
//! daemon can be driven and debugged with `nc -U` and a keyboard, which matters a lot for
//! something whose failure mode is "it silently stopped noticing slaps".

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod config;

pub use config::{Action, ActionKind, DaemonConfig, SoundOrder, SCHEMA_VERSION};

/// Where the daemon listens.
///
/// A user-owned directory rather than `/tmp`, so another local user cannot pre-create the
/// path and intercept the socket. Comfortably inside the 104-byte `sun_path` limit for any
/// plausible home directory.
pub fn socket_path() -> PathBuf {
    state_dir().join("yamete.sock")
}

/// Where config, logs and the socket live.
///
/// Requires `HOME`. Falling back to `/tmp` would put the control socket on a world-writable
/// parent where another local user can pre-create the path — fail loudly instead.
pub fn state_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/empty"));
    home.join("Library/Application Support/com.chipcolate.yamete")
}

pub fn config_path() -> PathBuf {
    state_dir().join("config.json")
}

/// A command from a client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Liveness check.
    Ping,
    /// Ask for the current configuration.
    GetConfig,
    /// Replace the configuration and persist it.
    SetConfig { config: Box<DaemonConfig> },
    /// Turn detection on or off without unloading the daemon.
    SetEnabled { value: bool },
    /// Start or stop receiving events on this connection.
    ///
    /// Telemetry is opt-in because it is the only expensive thing the daemon does: with no
    /// subscriber it never allocates or serialises a frame.
    Subscribe {
        #[serde(default)]
        slaps: bool,
        #[serde(default)]
        telemetry: bool,
    },
    /// Run one action as if a slap had been detected, for previewing it in the UI.
    TestAction {
        id: String,
        #[serde(default = "half")]
        intensity: f32,
    },
    /// Current daemon status.
    GetStatus,
}

fn half() -> f32 {
    0.5
}

/// A message from the daemon to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Pong,
    Config {
        config: Box<DaemonConfig>,
    },
    Status(Status),
    /// A slap was detected.
    Slap(Slap),
    /// A batch of decimated samples and detector scores, for the live scope.
    Telemetry(Telemetry),
    Error {
        message: String,
    },
}

/// A detected slap, as reported to clients.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Slap {
    /// Seconds since the daemon started streaming.
    pub t: f64,
    pub tier: yamete_dsp::Tier,
    pub peak_g: f32,
    pub intensity: f32,
    pub votes: u8,
    pub gyro_confirmed: bool,
    pub gyro_peak: f32,
    pub gyro_ratio: f32,
    pub axis: [f32; 3],
}

impl From<yamete_dsp::Detection> for Slap {
    fn from(d: yamete_dsp::Detection) -> Self {
        Slap {
            t: d.t,
            tier: d.tier,
            peak_g: d.peak_g,
            intensity: d.intensity,
            votes: d.votes,
            gyro_confirmed: d.gyro_confirmed,
            gyro_peak: d.gyro_peak,
            gyro_ratio: d.gyro_ratio,
            axis: d.axis,
        }
    }
}

/// One frame of live scope data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Telemetry {
    /// Seconds since streaming started, at the end of this batch.
    pub t: f64,
    /// Decimated high-passed acceleration envelope, in g.
    pub envelope: Vec<f32>,
    /// Decimated angular rate magnitude, in deg/s.
    pub gyro: Vec<f32>,
    /// Detector scores as of the end of the batch.
    pub scores: yamete_dsp::Scores,
    /// Samples the hardware produced that never reached us, cumulative.
    pub dropped: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub enabled: bool,
    pub version: String,
    /// Whether the gyroscope is present and streaming.
    pub has_gyro: bool,
    /// Seconds the daemon has been running.
    pub uptime_s: f64,
    /// Slaps detected since start.
    pub slaps: u64,
    /// Measured accelerometer sample rate.
    pub rate_hz: f32,
    /// Still building the background estimate.
    pub warming_up: bool,
    /// How many clients are currently receiving telemetry.
    ///
    /// Exposed because "the scope is blank" has two very different causes — nobody asked
    /// for telemetry, or it was asked for and lost — and they are indistinguishable from
    /// the outside without this.
    #[serde(default)]
    pub telemetry_subscribers: usize,
}

/// Serialise one message as a protocol line, newline included.
pub fn to_line<T: Serialize>(value: &T) -> String {
    let mut s = serde_json::to_string(value)
        .unwrap_or_else(|e| format!(r#"{{"event":"error","message":"could not serialise: {e}"}}"#));
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_fits_in_sun_path() {
        // `sockaddr_un.sun_path` is 104 bytes on macOS; exceeding it fails at bind time
        // with a confusing error, so it is worth knowing statically.
        let len = socket_path().as_os_str().len();
        assert!(len < 104, "socket path is {len} bytes: {:?}", socket_path());
    }

    #[test]
    fn state_dir_is_always_absolute() {
        // Prefer HOME; if it is missing we still refuse a relative path (a `/tmp` fallback
        // would be hijackable by another local user).
        assert!(state_dir().is_absolute());
    }

    #[test]
    fn daemon_config_json_shape_matches_the_ts_mirror() {
        // Hand-written app/src/types.ts. If this fails, update the TypeScript types in the
        // same change — do not "fix" the test by loosening it.
        let v = serde_json::to_value(DaemonConfig::default()).unwrap();
        let obj = v.as_object().unwrap();
        for key in [
            "schema_version",
            "enabled",
            "detector",
            "actions",
            "report_interval_us",
        ] {
            assert!(obj.contains_key(key), "missing top-level key `{key}`");
        }
        assert_eq!(obj["schema_version"], SCHEMA_VERSION);

        let det = obj["detector"].as_object().unwrap();
        for key in [
            "rate_hz",
            "highpass_hz",
            "sta_taus",
            "lta_tau",
            "thresholds",
            "tiers",
            "cooldown_s",
            "peak_hold_s",
            "cusum_leak",
            "gyro_mode",
            "gyro_ratio_min",
            "gyro_peak_min",
            "gyro_window_s",
            "min_votes",
            "full_scale_g",
            "sensitivity",
        ] {
            assert!(det.contains_key(key), "missing detector key `{key}`");
        }
    }

    #[test]
    fn request_and_event_tags_are_stable() {
        // Tag names are the contract with every client (app, nc, scripts).
        let req = to_line(&Request::Ping);
        assert!(req.contains(r#""cmd":"ping""#));
        let ev = to_line(&Event::Pong);
        assert!(ev.contains(r#""event":"pong""#));
    }

    #[test]
    fn legacy_config_without_schema_version_loads() {
        let raw = r#"{"enabled":true,"detector":{},"actions":[],"report_interval_us":1000}"#;
        let mut cfg: DaemonConfig = serde_json::from_str(raw).unwrap();
        cfg.normalize();
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn requests_round_trip() {
        let cases = vec![
            Request::Ping,
            Request::GetConfig,
            Request::SetEnabled { value: false },
            Request::Subscribe {
                slaps: true,
                telemetry: false,
            },
            Request::TestAction {
                id: "sound".into(),
                intensity: 0.9,
            },
        ];
        for c in cases {
            let line = to_line(&c);
            assert!(line.ends_with('\n'));
            let back: Request = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(format!("{c:?}"), format!("{back:?}"));
        }
    }

    #[test]
    fn subscribe_defaults_to_nothing() {
        // A bare `{"cmd":"subscribe"}` must not silently enable the expensive stream.
        let r: Request = serde_json::from_str(r#"{"cmd":"subscribe"}"#).unwrap();
        match r {
            Request::Subscribe { slaps, telemetry } => assert!(!slaps && !telemetry),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_action_intensity_defaults_to_midpoint() {
        let r: Request = serde_json::from_str(r#"{"cmd":"test_action","id":"x"}"#).unwrap();
        match r {
            Request::TestAction { intensity, .. } => assert_eq!(intensity, 0.5),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn events_are_tagged_for_clients() {
        assert_eq!(to_line(&Event::Pong).trim(), r#"{"event":"pong"}"#);

        let line = to_line(&Event::Error {
            message: "nope".into(),
        });
        assert!(line.contains(r#""event":"error""#));
        assert!(line.contains(r#""message":"nope""#));
    }

    #[test]
    fn slap_serialises_flat_for_easy_consumption() {
        let slap = Slap {
            t: 1.5,
            tier: yamete_dsp::Tier::Major,
            peak_g: 0.3,
            intensity: 0.8,
            votes: 4,
            gyro_confirmed: true,
            gyro_peak: 40.0,
            gyro_ratio: 500.0,
            axis: [0.0, 0.0, 1.0],
        };
        let line = to_line(&Event::Slap(slap));
        assert!(line.contains(r#""event":"slap""#));
        assert!(line.contains(r#""tier":"major""#));
        assert!(line.contains(r#""intensity":0.8"#));
    }

    #[test]
    fn unknown_commands_fail_loudly_rather_than_silently() {
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"self_destruct"}"#).is_err());
        assert!(serde_json::from_str::<Request>("not json").is_err());
    }
}
